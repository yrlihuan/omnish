// crates/omnish-client/src/main.rs
mod chat_session;
mod client_plugin;
mod command;
mod completion;
pub mod event_log;
mod ghost_complete;
mod display;
mod interceptor;
mod markdown;
mod probe;
mod shell_hook;
mod shell_input;
mod throttle;
mod util;
mod onboarding;
mod widgets;

use anyhow::Result;
use omnish_common::config::load_client_config;
use interceptor::{InputInterceptor, InterceptAction, TimeGapGuard};
use widgets::line_status::LineStatus;
use omnish_protocol::message::*;
use omnish_pty::proxy::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;
use omnish_transport::rpc_client::RpcClient;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

type MessageBuffer = Arc<Mutex<VecDeque<Message>>>;

const MAX_BUFFER_SIZE: usize = 10_000;

fn should_buffer(msg: &Message) -> bool {
    matches!(msg, Message::IoData(_) | Message::CommandComplete(_) | Message::SessionUpdate(_))
}

/// Send completion summary to daemon if there's a pending completion
fn send_completion_summary(
    rpc: &RpcClient,
    shell_completer: &mut completion::ShellCompleter,
    session_id: &str,
    accepted: bool,
    cwd: Option<String>,
) {
    if let Some(summary) = shell_completer.take_completion_summary(session_id, accepted, cwd) {
        let rpc = rpc.clone();
        let msg = Message::CompletionSummary(summary);
        tokio::spawn(async move {
            let _ = rpc.send(msg).await;
        });
    }
}

/// Send completion summary for ignored completion (accepted=false)
fn send_ignored_summary(
    rpc: &RpcClient,
    shell_completer: &mut completion::ShellCompleter,
    session_id: &str,
    cwd: Option<String>,
) {
    // take_completion_summary returns None if there's no pending completion
    send_completion_summary(rpc, shell_completer, session_id, false, cwd);
}

/// Send a message to the daemon, buffering it if the send fails and
/// the message type is eligible for retry.
async fn send_or_buffer(rpc: &RpcClient, msg: Message, buffer: &MessageBuffer) {
    if rpc.send(msg.clone()).await.is_err() && should_buffer(&msg) {
        let mut buf = buffer.lock().await;
        if buf.len() >= MAX_BUFFER_SIZE {
            buf.pop_front();
        }
        buf.push_back(msg);
    }
}

fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Get the shell's current working directory
pub(crate) fn get_shell_cwd(pid: u32) -> Option<String> {
    util::get_shell_cwd(pid)
}

/// Resolve the real shell to spawn, avoiding infinite recursion when omnish
/// itself is set as $SHELL (e.g. in tmux `default-shell`).
fn resolve_shell(config_shell: &str) -> String {
    let candidate = std::env::var("SHELL").unwrap_or_else(|_| config_shell.to_string());
    let exe_name = std::path::Path::new(&candidate)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if exe_name.starts_with("omnish") {
        // $SHELL points to omnish — fall back to config, then common defaults
        if !config_shell.is_empty()
            && !std::path::Path::new(config_shell)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .starts_with("omnish")
        {
            return config_shell.to_string();
        }
        // Try common shells
        for fallback in &["/bin/bash", "/bin/zsh", "/bin/sh"] {
            if std::path::Path::new(fallback).exists() {
                return fallback.to_string();
            }
        }
        "/bin/sh".to_string()
    } else {
        candidate
    }
}

struct ResumeArgs {
    master_fd: i32,
    child_pid: i32,
    session_id: String,
    cursor_col: u16,
    cursor_row: u16,
}

fn parse_resume_args() -> Option<ResumeArgs> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--resume") {
        return None;
    }
    let fd = args.iter()
        .find_map(|a| a.strip_prefix("--fd="))
        .and_then(|v| v.parse::<i32>().ok())?;
    let pid = args.iter()
        .find_map(|a| a.strip_prefix("--pid="))
        .and_then(|v| v.parse::<i32>().ok())?;
    let sid = args.iter()
        .find_map(|a| a.strip_prefix("--session-id="))?
        .to_string();
    let cursor_col = args.iter()
        .find_map(|a| a.strip_prefix("--cursor-col="))
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    let cursor_row = args.iter()
        .find_map(|a| a.strip_prefix("--cursor-row="))
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    Some(ResumeArgs { master_fd: fd, child_pid: pid, session_id: sid, cursor_col, cursor_row })
}

mod notice_queue {
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::sync::Mutex;

    static DEFERRED: AtomicBool = AtomicBool::new(false);
    static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// Current cursor row, updated by CursorTracker.
    static CURSOR_ROW: AtomicU16 = AtomicU16::new(0);

    /// Update the tracked cursor row (called from CursorTracker after feed).
    pub fn set_cursor_row(row: u16) {
        CURSOR_ROW.store(row, Ordering::Relaxed);
    }

    /// Queue a notice. If deferred mode is on, store it; otherwise display immediately.
    pub fn push(msg: &str) {
        if DEFERRED.load(Ordering::Relaxed) {
            if let Ok(mut q) = QUEUE.lock() {
                q.push(msg.to_string());
            }
        } else {
            render(msg);
        }
    }

    /// Enable deferred mode (e.g. when entering chat).
    pub fn defer() {
        DEFERRED.store(true, Ordering::Relaxed);
    }

    /// Disable deferred mode and flush all queued notices.
    pub fn flush() {
        DEFERRED.store(false, Ordering::Relaxed);
        let msgs: Vec<String> = {
            match QUEUE.lock() {
                Ok(mut q) => q.drain(..).collect(),
                Err(_) => return,
            }
        };
        for msg in msgs {
            render(&msg);
        }
    }

    fn render(msg: &str) {
        use crate::widgets::inline_notice::InlineNotice;
        let cols = super::get_terminal_size().map(|(_, c)| c as usize).unwrap_or(80);
        let at_bottom = CURSOR_ROW.load(Ordering::Relaxed) > 0;
        let debug_msg = format!("[{}] {}", at_bottom as u8, msg);
        eprint!("{}", InlineNotice::render_at(&debug_msg, cols, at_bottom));
    }
}

fn notice(msg: &str) {
    notice_queue::push(msg);
}

fn exec_update(proxy: &PtyProxy, session_id: &str, cursor_col: u16, cursor_row: u16) {
    let current_exe = match std::env::current_exe() {
        Ok(p) => {
            // On Linux, /proc/self/exe appends " (deleted)" when the binary was replaced on disk.
            // Strip the suffix to get the actual install path where the new binary lives.
            let s = p.to_string_lossy().to_string();
            if let Some(stripped) = s.strip_suffix(" (deleted)") {
                std::path::PathBuf::from(stripped)
            } else {
                p
            }
        }
        Err(e) => {
            notice(&format!("[omnish] Failed to resolve current exe: {}", e));
            return;
        }
    };

    if !current_exe.exists() {
        notice(&format!("[omnish] Binary not found: {}", current_exe.display()));
        return;
    }

    // Get on-disk binary version by running it with --version
    let disk_version = match std::process::Command::new(&current_exe)
        .arg("--version")
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => {
            notice(&format!("[omnish] Failed to check binary version: {}", e));
            return;
        }
    };

    let running_version = format!("omnish {}", omnish_common::VERSION);
    if disk_version == running_version {
        notice(&format!("[omnish] Already up to date ({})", omnish_common::VERSION));
        return;
    }

    notice(&format!("[omnish] Updating: {} -> {}", running_version, disk_version));

    // On macOS (especially Apple Silicon), all binaries must be code-signed.
    // When the binary is copied/replaced on disk, the signature may be lost.
    // Re-sign with ad-hoc signature before running to avoid SIGKILL ("Killed: 9").
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&current_exe)
            .output();
    }

    // Clear FD_CLOEXEC on the PTY master fd so it survives exec
    let master_fd = proxy.master_raw_fd();
    unsafe {
        let flags = libc::fcntl(master_fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(master_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }

    // Build args for the new process
    let exe_cstr = std::ffi::CString::new(current_exe.to_string_lossy().as_bytes()).unwrap();
    let args = [
        exe_cstr.clone(),
        std::ffi::CString::new("--resume").unwrap(),
        std::ffi::CString::new(format!("--fd={}", master_fd)).unwrap(),
        std::ffi::CString::new(format!("--pid={}", proxy.child_pid())).unwrap(),
        std::ffi::CString::new(format!("--session-id={}", session_id)).unwrap(),
        std::ffi::CString::new(format!("--cursor-col={}", cursor_col)).unwrap(),
        std::ffi::CString::new(format!("--cursor-row={}", cursor_row)).unwrap(),
    ];

    // execvp replaces this process — only returns on error
    let _ = nix::unistd::execvp(&exe_cstr, &args);
    notice(&format!("[omnish] exec failed: {}", std::io::Error::last_os_error()));
}

#[tokio::main(worker_threads = 4)]
async fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("omnish {}", omnish_common::VERSION);
        return Ok(());
    }

    let config = load_client_config().unwrap_or_default();
    let onboarded = Arc::new(AtomicBool::new(config.onboarded));
    let resume_args = parse_resume_args();

    // If stdin is not a terminal (e.g. rsync over SSH, piped commands),
    // exec the underlying shell directly — omnish requires a PTY.
    if resume_args.is_none() && !nix::unistd::isatty(0).unwrap_or(false) {
        let shell = resolve_shell(&config.shell.command);
        let shell_cstr = std::ffi::CString::new(shell.as_str()).expect("shell path");
        // Pass through any arguments after argv[0]
        let mut args: Vec<std::ffi::CString> = vec![shell_cstr.clone()];
        for arg in std::env::args().skip(1) {
            args.push(std::ffi::CString::new(arg).expect("arg"));
        }
        nix::unistd::execvp(&shell_cstr, &args)?;
        unreachable!();
    }

    let (session_id, proxy, osc133_hook_installed) = if let Some(ref resume) = resume_args {
        // Set cursor row early so resume notice uses correct rendering mode
        notice_queue::set_cursor_row(resume.cursor_row);
        // Resume mode: reconstruct PtyProxy from passed fd/pid
        let proxy = unsafe { PtyProxy::from_raw_fd(resume.master_fd, resume.child_pid) };
        notice(&format!("[omnish] Resumed (pid={}, fd={})", resume.child_pid, resume.master_fd));
        (resume.session_id.clone(), proxy, true)
    } else {
        // Normal startup: spawn a new shell
        let session_id = Uuid::new_v4().to_string()[..8].to_string();
        let shell = resolve_shell(&config.shell.command);

        let mut child_env = HashMap::new();
        child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
        child_env.insert("SHELL".to_string(), shell.clone());

        let osc133_rcfile = shell_hook::install_bash_hook(&shell);
        let osc133_hook_installed = osc133_rcfile.is_some();
        let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
            vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
        } else {
            vec![]
        };
        let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();
        let proxy = PtyProxy::spawn_with_env(&shell, &shell_args_ref, child_env)?;
        // Print welcome message for first-time users
        if !config.onboarded {
            onboarding::print_welcome();
        }
        (session_id, proxy, osc133_hook_installed)
    };
    let parent_session_id = std::env::var("OMNISH_SESSION_ID").ok();
    let daemon_addr = std::env::var("OMNISH_SOCKET")
        .unwrap_or_else(|_| config.daemon_addr.clone());

    // Connect to daemon (graceful degradation)
    let pending_buffer: MessageBuffer = Arc::new(Mutex::new(VecDeque::new()));
    let daemon_conn = connect_daemon(&daemon_addr, &session_id, parent_session_id, proxy.child_pid() as u32, pending_buffer.clone()).await;

    // Spawn shell info polling task (progressive interval: 1/2/4/8/15/30s, then 60s)
    // Reset to 1s on each command start
    let (cmd_start_tx, mut cmd_start_rx) = mpsc::channel::<()>(1);
    let cmd_start_tx_for_loop = cmd_start_tx.clone();
    if let Some(ref rpc) = daemon_conn {
        let rpc_poll = rpc.clone();
        let sid_poll = session_id.clone();
        let child_pid_poll = proxy.child_pid() as u32;
        let poll_buffer = pending_buffer.clone();
        tokio::spawn(async move {
            // Polling probes include: hostname, shell_cwd, child_process
            let polling_probes = probe::default_polling_probes(child_pid_poll);

            let mut last_attrs: HashMap<String, String> = HashMap::new();

            // Progressive polling intervals: 1, 2, 4, 8, 15, 30, then 60 seconds
            let intervals: &[u64] = &[1, 2, 4, 8, 15, 30, 60];
            let mut interval_idx: usize = 0;

            loop {
                let current_interval = intervals.get(interval_idx).unwrap_or(&60);

                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(*current_interval)) => {
                        // Time to poll
                    }
                    _ = cmd_start_rx.recv() => {
                        // Command started - reset to first interval
                        interval_idx = 0;
                        continue; // Skip this poll, start fresh with 1s interval
                    }
                }

                // Collect all probes: hostname, shell_cwd, child_process
                let current = polling_probes.collect_all();

                // Diff: find changed keys
                let changed: HashMap<String, String> = current.iter()
                    .filter(|(k, v)| last_attrs.get(*k) != Some(v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();

                if !changed.is_empty() {
                    // Update tmux window title if child_process changed
                    if let Some(child_process) = changed.get("child_process") {
                        let in_tmux = std::env::var("TMUX").is_ok();
                        let process_name = child_process.split(':').next().unwrap_or(child_process.as_str());
                        if let Some(title) = tmux_title(process_name, in_tmux) {
                            nix::unistd::write(std::io::stdout(), title.as_bytes()).ok();
                        }
                    }

                    let msg = Message::SessionUpdate(SessionUpdate {
                        session_id: sid_poll.clone(),
                        timestamp_ms: timestamp_ms(),
                        attrs: changed,
                    });
                    send_or_buffer(&rpc_poll, msg, &poll_buffer).await;
                }

                last_attrs = current;

                // Move to next interval (but cap at the last interval = 60s)
                if interval_idx < intervals.len() - 1 {
                    interval_idx += 1;
                }
            }
        });
    }

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
    let guard = TimeGapGuard::new(std::time::Duration::from_millis(config.shell.intercept_gap_ms));
    let mut interceptor = InputInterceptor::new(&config.shell.command_prefix, &config.shell.resume_prefix, Box::new(guard));
    let prefix_bytes = config.shell.command_prefix.as_bytes();
    let mut alt_screen_detector = AltScreenDetector::new();
    let mut col_tracker = if let Some(ref r) = resume_args {
        let t = CursorTracker::with_position(r.cursor_col, r.cursor_row);
        notice_queue::set_cursor_row(t.row);
        t
    } else {
        CursorTracker::new()
    };
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
    let mut command_tracker = omnish_tracker::command_tracker::CommandTracker::new(
        session_id.clone(), cwd,
    );
    let mut throttle = throttle::OutputThrottle::new();
    let mut osc133_detector = omnish_tracker::osc133_detector::Osc133Detector::new();
    let mut dsr_detector = DsrDetector::new();
    let mut osc133_warned = false;
    let mut no_readline_warned = false;
    let mut completer = ghost_complete::GhostCompleter::new(vec![
        Box::new(ghost_complete::BuiltinProvider::new()),
    ]);
    let mut shell_input = shell_input::ShellInputTracker::new();
    let mut last_readline_content: Option<String> = None;
    // Pending completion responses waiting for readline report
    let mut pending_completion_responses: Vec<omnish_protocol::message::CompletionResponse> = Vec::new();
    // Whether we've triggered a readline report for pending completions
    let mut readline_triggered_for_completions = false;
    // Deferred ghost text render — rendered after next PTY display_data write
    // so bash's readline redraw (after bind-x hook) doesn't overwrite it.
    let mut deferred_ghost: Option<String> = None;
    // When we triggered readline report (for timeout)
    let mut readline_trigger_time: Option<std::time::Instant> = None;
    let in_tmux = std::env::var("TMUX").is_ok();
    if let Some(title) = tmux_title("omnish", in_tmux) {
        nix::unistd::write(std::io::stdout(), title.as_bytes()).ok();
    }
    let mut shell_completer = completion::ShellCompleter::new();
    let (completion_tx, mut completion_rx) = tokio::sync::mpsc::channel::<
        omnish_protocol::message::CompletionResponse
    >(4);
    // Chat command history persists across chat sessions within same client
    let mut chat_history: VecDeque<String> = VecDeque::with_capacity(100);
    let mut last_thread_id: Option<String> = None;

    // Auto-update state
    let auto_update_enabled = Arc::new(AtomicBool::new(config.auto_update));
    let mut exe_mtime = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let s = p.to_string_lossy().to_string();
            let clean = s.strip_suffix(" (deleted)").map(std::path::PathBuf::from).unwrap_or(p);
            std::fs::metadata(&clean).ok()?.modified().ok()
        });
    let mut last_keystroke = std::time::Instant::now();
    let mut last_update_check = std::time::Instant::now();
    const AUTO_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
    const AUTO_UPDATE_IDLE: std::time::Duration = std::time::Duration::from_secs(60);

    // Tracks when prefix was matched, for timing-based detection of
    // double-prefix (e.g. "::") vs single prefix (":").
    let mut prefix_match_time: Option<std::time::Instant> = None;
    const PREFIX_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

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

        let poll_start = std::time::Instant::now();
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
        if ret < 0 {
            continue;
        }

        // Auto-update check (every 60s, only when idle at prompt for 60s+)
        if auto_update_enabled.load(Ordering::Relaxed)
            && last_update_check.elapsed() >= AUTO_UPDATE_INTERVAL
            && shell_input.at_prompt()
            && last_keystroke.elapsed() >= AUTO_UPDATE_IDLE
            && !interceptor.is_in_chat()
            && !alt_screen_detector.is_active()
        {
            last_update_check = std::time::Instant::now();
            if let Some(ref startup_mtime) = exe_mtime {
                let current_mtime = std::env::current_exe()
                    .ok()
                    .and_then(|p| {
                        let s = p.to_string_lossy().to_string();
                        let clean = s.strip_suffix(" (deleted)").map(std::path::PathBuf::from).unwrap_or(p);
                        std::fs::metadata(&clean).ok()?.modified().ok()
                    });
                if let Some(current) = current_mtime {
                    if current != *startup_mtime {
                        exec_update(&proxy, &session_id, col_tracker.col, col_tracker.row);
                        // exec_update only returns on error — reset timer
                    }
                    // Update mtime after check to avoid repeated unnecessary checks
                    exe_mtime = current_mtime;
                }
            }
        }

        // Check if prefix-match timeout expired (": " waited long enough → enter new chat)
        if let Some(t) = prefix_match_time {
            if t.elapsed() >= PREFIX_TIMEOUT {
                prefix_match_time = None;
                if let Some(action) = interceptor.expire_prefix() {
                    if matches!(action, InterceptAction::Chat(_)) {
                        // notice_queue::push(&format!(": timeout ({}ms)", t.elapsed().as_millis()));
                        event_log::push("chat mode enter (timeout)");
                        notice_queue::defer();
                        completer.clear();
                        let saved_input = shell_input.input().to_string();
                        if let Some(ref rpc) = daemon_conn {
                            let shell_pid = proxy.child_pid() as u32;
                            let dbg_fn = || debug_client_state(
                                &shell_input, &interceptor, &shell_completer,
                                &daemon_conn, &osc133_detector, &last_readline_content,
                                shell_pid, &col_tracker,
                            );
                            {
                                let mut session = chat_session::ChatSession::new(std::mem::take(&mut chat_history));
                                session.run(rpc, &session_id, &proxy, None, &dbg_fn, &auto_update_enabled, &onboarded, col_tracker.col, col_tracker.row).await;
                                chat_history = session.into_history();
                            }
                        } else {
                            let err = display::render_error("Daemon not connected");
                            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        }
                        nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                        notice_queue::flush();
                        proxy.write_all(b"\x15\x0b\r").ok();
                        if !saved_input.is_empty() {
                            proxy.write_all(saved_input.as_bytes()).ok();
                        }
                    }
                }
            }
        }

        // Stdin -> PTY master
        if fds[0].revents & libc::POLLIN != 0 {
            let n = nix::unistd::read(0, &mut input_buf)?;
            if n == 0 {
                break;
            }
            last_keystroke = std::time::Instant::now();
            deferred_ghost = None; // User typed — cancel pending ghost render

            // Suppress interceptor when not at prompt (child process running:
            // ssh, python REPL, etc.) so ':' is forwarded to the child.
            // Alt screen detector is handled separately in the output path.
            if !alt_screen_detector.is_active() {
                interceptor.set_suppressed(!shell_input.at_prompt());
            }

            // Filter DSR responses from stdin, then feed remaining bytes to interceptor.
            let mut filtered_input: Vec<u8> = Vec::new();
            for &byte in &input_buf[..n] {
                match dsr_detector.feed(byte) {
                    Some(Some((row, col))) => {
                        // DSR response complete: update cursor tracker
                        col_tracker.row = row.saturating_sub(1); // 1-based → 0-based
                        col_tracker.col = col.saturating_sub(1);
                        notice_queue::set_cursor_row(col_tracker.row);
                    }
                    Some(None) => {
                        // Byte consumed, still accumulating DSR response
                    }
                    None => {
                        // Not a DSR byte — check if detector aborted mid-sequence
                        if !dsr_detector.buf.is_empty() {
                            // Replay buffered bytes (includes current byte already)
                            let replay = dsr_detector.take_buf();
                            filtered_input.extend_from_slice(&replay);
                        } else {
                            // Normal byte, not part of any DSR sequence
                            filtered_input.push(byte);
                        }
                    }
                }
            }

            // Flush bare ESC from DSR detector — a standalone ESC not followed
            // by '[' in the same read() is a user keypress, not a DSR response.
            if let Some(flushed) = dsr_detector.flush_bare_esc() {
                filtered_input.extend_from_slice(&flushed);
            }
            for &byte in &filtered_input {
                match interceptor.feed_byte(byte) {
                    InterceptAction::Buffering(buf) => {
                        if buf == prefix_bytes {
                            // Full prefix matched — start timer for double-prefix detection.
                            // No visual feedback yet; chat prompt appears on timeout or Enter.
                            shell_completer.clear();
                            prefix_match_time = Some(std::time::Instant::now());
                        } else if buf.len() > prefix_bytes.len() && buf.starts_with(prefix_bytes) {
                            // Additional input after prefix — cancel timer
                            prefix_match_time = None;
                        }
                    }
                    InterceptAction::Backspace(_buf) => {
                        // No visual prompt to update — prefix buffering is invisible
                    }
                    InterceptAction::Forward(bytes) => {
                        // Check if Tab should be intercepted for shell completion
                        if bytes == [b'\t'] && shell_completer.ghost().is_some() {
                            if let Some(suffix) = shell_completer.accept() {
                                event_log::push(format!("completion accepted suffix={suffix:?}"));
                                // Safety: if cursor is not at end, move to end first
                                if !shell_input.cursor_at_end() {
                                    proxy.write_all(b"\x05")?; // Ctrl-E: move to end of line
                                }
                                proxy.write_all(suffix.as_bytes())?;
                                shell_input.inject(&suffix);
                                command_tracker.feed_input(suffix.as_bytes(), timestamp_ms());
                                // Send completion summary (accepted)
                                if let Some(ref rpc) = daemon_conn {
                                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                    send_completion_summary(rpc, &mut shell_completer, &session_id, true, shell_cwd);
                                }
                            }
                        } else if bytes == [0x1b] && shell_completer.ghost().is_some() {
                            // Bare ESC dismisses ghost text — consume the key (don't forward to PTY)
                            if shell_completer.dismiss() {
                                nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                if let Some(ref rpc) = daemon_conn {
                                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                    send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
                                }
                            }
                        } else {
                            // Forward these bytes to PTY
                            proxy.write_all(&bytes)?;

                            if shell_input.at_prompt() {
                                if needs_readline_report(&bytes) {
                                    // Tab, Up, Down modify readline state — send
                                    // trigger so bash reports the real READLINE_LINE.
                                    // Note: Tab trigger removed here - now triggered on completion response
                                    // to avoid interfering with bash completion list display (issue #23)
                                    // Skip trigger if already pending (e.g. isearch mode from Ctrl+R)
                                    // to avoid "cannot find keymap for command" error (issue #49)
                                    if osc133_hook_installed && !shell_input.pending_rl_report() {
                                        event_log::push("readline request (input key)");
                                        proxy.write_all(b"\x1b[13337~")?;
                                    }
                                    shell_input.mark_pending_report();
                                    if shell_completer.ghost().is_some() {
                                        // Send completion summary (ignored - user pressed Tab/Up/Down)
                                        if let Some(ref rpc) = daemon_conn {
                                            let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                            send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
                                        }
                                        shell_completer.clear();
                                        nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                    }
                                } else if bytes.contains(&0x12) {
                                    // Ctrl+R enters isearch mode (different keymap)
                                    // so we can't send the trigger, but still suppress
                                    // stale completions until the next prompt event.
                                    event_log::push("ctrl+r (isearch mode)");
                                    shell_input.enter_isearch();
                                    shell_input.mark_pending_report();
                                    if shell_completer.ghost().is_some() {
                                        // Send completion summary (ignored - user pressed Ctrl+R)
                                        if let Some(ref rpc) = daemon_conn {
                                            let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                            send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
                                        }
                                        shell_completer.clear();
                                        nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                    }
                                }
                            }

                            // Track shell input for LLM completion
                            shell_input.feed_forwarded(&bytes);
                            // Always reset debounce on input activity, even if
                            // take_change() returns None due to pending_rl_report
                            shell_completer.note_activity();
                            if let Some((input, seq)) = shell_input.take_change() {
                                if shell_completer.on_input_changed(input, seq) {
                                    // Ghost was cleared — erase stale ghost text from screen
                                    nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                    // Send completion summary (ignored - user typed different input)
                                    if let Some(ref rpc) = daemon_conn {
                                        let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                        send_ignored_summary(rpc, &mut shell_completer, &session_id, shell_cwd);
                                    }
                                }
                            }

                            // Feed input to command tracker
                            command_tracker.feed_input(&bytes, timestamp_ms());

                            // Report to daemon async (skip during alt screen)
                            if let Some(ref rpc) = daemon_conn {
                                if !alt_screen_detector.is_active() {
                                    let msg = Message::IoData(IoData {
                                        session_id: session_id.clone(),
                                        direction: IoDirection::Input,
                                        timestamp_ms: timestamp_ms(),
                                        data: bytes,
                                    });
                                    send_or_buffer(rpc, msg, &pending_buffer).await;
                                }
                            }
                        }
                    }
                    InterceptAction::Cancel => {
                        // ESC pressed — reset state, no UI to dismiss
                        prefix_match_time = None;
                        completer.clear();
                    }
                    InterceptAction::Chat(msg) => {
                        prefix_match_time = None;
                        event_log::push("chat mode enter");
                        notice_queue::defer();
                        completer.clear();
                        // Save pre-chat input to restore after chat (issue #24)
                        let saved_input = shell_input.input().to_string();

                        // Enter chat mode loop (pass initial message if any)
                        if let Some(ref rpc) = daemon_conn {
                            let initial = if msg.trim().is_empty() { None } else { Some(msg) };
                            let shell_pid = proxy.child_pid() as u32;
                            let dbg_fn = || debug_client_state(
                                &shell_input,
                                &interceptor,
                                &shell_completer,
                                &daemon_conn,
                                &osc133_detector,
                                &last_readline_content,
                                shell_pid,
                                &col_tracker,
                            );
                            {
                                let mut session = chat_session::ChatSession::new(std::mem::take(&mut chat_history));
                                session.run(rpc, &session_id, &proxy, initial, &dbg_fn, &auto_update_enabled, &onboarded, col_tracker.col, col_tracker.row).await;
                                last_thread_id = session.thread_id().map(String::from);
                                chat_history = session.into_history();
                            }
                        } else {
                            let err = display::render_error("Daemon not connected");
                            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        }

                        // Immediately erase the chat "> " prompt for instant feedback
                        nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();

                        // Flush deferred notices now that we're back in command mode
                        notice_queue::flush();

                        // Clear bash readline before restoring.
                        // Ctrl-U (kill backward) + Ctrl-K (kill forward) + Enter
                        // to clear regardless of cursor position (issue #125).
                        proxy.write_all(b"\x15\x0b\r").ok();
                        // Restore pre-chat input so user doesn't lose their work (issue #24)
                        if !saved_input.is_empty() {
                            proxy.write_all(saved_input.as_bytes()).ok();
                        }
                    }
                    InterceptAction::ResumeChat => {
                        let gap_ms = prefix_match_time.map(|t| t.elapsed().as_millis()).unwrap_or(0);
                        prefix_match_time = None;
                        // notice_queue::push(&format!(":: detected (gap {}ms)", gap_ms));
                        event_log::push(format!("chat mode resume (double-prefix, gap {}ms)", gap_ms));
                        notice_queue::defer();
                        completer.clear();
                        let saved_input = shell_input.input().to_string();

                        if let Some(ref rpc) = daemon_conn {
                            let shell_pid = proxy.child_pid() as u32;
                            let dbg_fn = || debug_client_state(
                                &shell_input,
                                &interceptor,
                                &shell_completer,
                                &daemon_conn,
                                &osc133_detector,
                                &last_readline_content,
                                shell_pid,
                                &col_tracker,
                            );
                            {
                                let resume_cmd = match last_thread_id {
                                    Some(ref tid) => format!("/resume_tid {}", tid),
                                    None => "/resume 1".to_string(),
                                };
                                let mut session = chat_session::ChatSession::new(std::mem::take(&mut chat_history));
                                session.run(rpc, &session_id, &proxy, Some(resume_cmd), &dbg_fn, &auto_update_enabled, &onboarded, col_tracker.col, col_tracker.row).await;
                                last_thread_id = session.thread_id().map(String::from);
                                chat_history = session.into_history();
                            }
                        } else {
                            let err = display::render_error("Daemon not connected");
                            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        }

                        nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                        notice_queue::flush();
                        proxy.write_all(b"\x15\x0b\r").ok();
                        if !saved_input.is_empty() {
                            proxy.write_all(saved_input.as_bytes()).ok();
                        }
                    }
                    InterceptAction::Tab(_buf) => {
                        // Check if completer has a suggestion to accept
                        if let Some(suffix) = completer.accept() {
                            for &b in suffix.as_bytes() {
                                interceptor.inject_byte(b);
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
                match action {
                    InterceptAction::Cancel => {
                        prefix_match_time = None;
                        completer.clear();
                    }
                    InterceptAction::Forward(bytes) => {
                        // Bare ESC dismisses ghost text — consume the key
                        if bytes == [0x1b] && shell_completer.ghost().is_some() {
                            if shell_completer.dismiss() {
                                nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                if let Some(ref rpc) = daemon_conn {
                                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                    send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
                                }
                            }
                        } else {
                            proxy.write_all(&bytes)?;
                            shell_input.feed_forwarded(&bytes);
                            command_tracker.feed_input(&bytes, timestamp_ms());
                            if let Some(ref rpc) = daemon_conn {
                                if !alt_screen_detector.is_active() {
                                    let msg = Message::IoData(IoData {
                                        session_id: session_id.clone(),
                                        direction: IoDirection::Input,
                                        timestamp_ms: timestamp_ms(),
                                        data: bytes,
                                    });
                                    send_or_buffer(rpc, msg, &pending_buffer).await;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            let input_elapsed = poll_start.elapsed();
            if input_elapsed.as_millis() > 50 {
                event_log::push(format!("input lag {}ms ({}B)", input_elapsed.as_millis(), n));
            }
        }

        // PTY master -> stdout
        if fds[1].revents & libc::POLLIN != 0 {
            match proxy.read(&mut output_buf) {
                Ok(0) => break,
                Ok(n) => {
                    let raw = &output_buf[..n];

                    // Detect OSC 133 events from raw output
                    let osc_events = osc133_detector.feed(raw);

                    // Strip OSC 133 sequences before displaying to user
                    let stripped;
                    let display_data: &[u8] = if osc_events.is_empty() {
                        &output_buf[..n]
                    } else {
                        stripped = omnish_tracker::osc133_detector::strip_osc133(raw);
                        &stripped
                    };

                    nix::unistd::write(std::io::stdout(), display_data)?;

                    // Render deferred ghost text after bash's readline redraw
                    if let Some(ghost_render) = deferred_ghost.take() {
                        nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                    }

                    // Track cursor position on display (stripped) data
                    col_tracker.feed(display_data);
                    notice_queue::set_cursor_row(col_tracker.row);

                    // Detect alternate screen transitions
                    if let Some(active) = alt_screen_detector.feed(display_data) {
                        interceptor.set_suppressed(active);
                    }

                    // Notify interceptor of output (resets chat state)
                    interceptor.note_output(display_data);

                    // Send IoData to daemon (throttled) — skip while alternate screen
                    // is active (vim, less, htop, etc.) to avoid storing TUI noise.
                    if let Some(ref rpc) = daemon_conn {
                        if !alt_screen_detector.is_active() && throttle.should_send(n) {
                            let msg = Message::IoData(IoData {
                                session_id: session_id.clone(),
                                direction: IoDirection::Output,
                                timestamp_ms: timestamp_ms(),
                                data: display_data.to_vec(),
                            });
                            send_or_buffer(rpc, msg, &pending_buffer).await;
                            throttle.record_sent(n);
                        }
                    }

                    // Feed OSC 133 events to command tracker
                    let mut completed = Vec::new();
                    use omnish_tracker::osc133_detector::Osc133EventKind;
                    for event in osc_events {
                        match &event.kind {
                            // 133;A (PromptStart) / 133;D (CommandEnd):
                            // prompt is being displayed → user can type
                            Osc133EventKind::PromptStart => {
                                event_log::push("osc133 PromptStart");
                                shell_input.on_prompt();
                                shell_completer.clear();
                                last_readline_content = None;
                                query_cursor_position();
                                if let Some(title) = tmux_title("omnish", in_tmux) {
                                    nix::unistd::write(std::io::stdout(), title.as_bytes()).ok();
                                }
                            }
                            Osc133EventKind::CommandEnd { exit_code } => {
                                event_log::push(format!("osc133 CommandEnd exit_code={exit_code}"));
                                shell_input.on_prompt();
                                shell_completer.clear();
                                last_readline_content = None;
                                query_cursor_position();
                                if let Some(title) = tmux_title("omnish", in_tmux) {
                                    nix::unistd::write(std::io::stdout(), title.as_bytes()).ok();
                                }
                            }
                            // 133;B / 133;C: In our bash hook these fire together
                            // from the DEBUG trap, which also triggers during PS1
                            // command substitution (e.g. git branch). So we can NOT
                            // use them to detect "user pressed Enter". Instead,
                            // at_prompt=false is set by feed_forwarded on Enter key.
                            Osc133EventKind::CommandStart { command, original, .. } => {
                                event_log::push(format!(
                                    "osc133 CommandStart cmd={} orig={}",
                                    command.as_deref().unwrap_or("(none)"),
                                    original.as_deref().unwrap_or("(none)"),
                                ));
                                // User pressed Enter - send completion summary (ignored)
                                if let Some(ref rpc) = daemon_conn {
                                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                    send_ignored_summary(rpc, &mut shell_completer, &session_id, shell_cwd);
                                }
                                shell_completer.clear();
                                // Reset polling interval to 1s on command start
                                let _ = cmd_start_tx_for_loop.try_send(());
                                if let Some(cmd) = command {
                                    if let Some(title) = tmux_title(command_basename(cmd), in_tmux) {
                                        nix::unistd::write(std::io::stdout(), title.as_bytes()).ok();
                                    }
                                }
                            }
                            Osc133EventKind::OutputStart => {
                                event_log::push("osc133 OutputStart");
                                // Output started - send completion summary (ignored)
                                if let Some(ref rpc) = daemon_conn {
                                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                    send_ignored_summary(rpc, &mut shell_completer, &session_id, shell_cwd);
                                }
                                shell_completer.clear();
                            }
                            Osc133EventKind::ReadlineLine { content, point } => {
                                event_log::push(format!(
                                    "readline response content={:?} point={:?}",
                                    content, point
                                ));
                                shell_input.set_readline(content, *point);
                                last_readline_content = Some(content.to_string());

                                // Process any pending completion responses now that we have latest input
                                if !pending_completion_responses.is_empty() {
                                    if shell_input.cursor_at_end() {
                                        let current = shell_input.input();
                                        for resp in pending_completion_responses.drain(..) {
                                            if let Some(ghost) = shell_completer.on_response(&resp, current) {
                                                // Defer rendering until after bash's readline
                                                // redraw (which arrives in the next PTY read).
                                                deferred_ghost = Some(display::render_ghost_text(ghost));
                                            }
                                        }
                                    } else {
                                        // Cursor not at end — discard pending completions
                                        pending_completion_responses.clear();
                                        if shell_completer.ghost().is_some() {
                                            shell_completer.clear();
                                            nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                        }
                                    }
                                    readline_triggered_for_completions = false;
                                    readline_trigger_time = None;
                                }

                                if let Some((input, seq)) = shell_input.take_change() {
                                    if shell_completer.on_input_changed(input, seq) {
                                        nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                                    }
                                }
                            }
                            Osc133EventKind::NoReadline => {
                                if !no_readline_warned {
                                    notice("[omnish] bash readline not available (bind -x unsupported). Completions disabled.");
                                    no_readline_warned = true;
                                }
                            }
                        }
                        let cmds = command_tracker.feed_osc133(event, timestamp_ms(), 0);
                        completed.extend(cmds);
                    }

                    // Feed output for regex fallback (returns empty when osc133_mode active)
                    let regex_cmds = command_tracker.feed_output(raw, timestamp_ms(), 0);
                    if !regex_cmds.is_empty() && osc133_hook_installed && !osc133_warned {
                        osc133_warned = true;
                        eprintln!("\x1b[31m[omnish]\x1b[0m OSC 133 shell hook not active, falling back to regex prompt detection");
                    }
                    completed.extend(regex_cmds);

                    // Feed raw output for summary collection in osc133 mode
                    command_tracker.feed_output_raw(raw, timestamp_ms(), 0);

                    // Send completed commands to daemon
                    for record in &completed {
                        event_log::push(format!(
                            "command complete: {:?} exit={:?}",
                            record.command_line, record.exit_code
                        ));
                        if let Some(ref rpc) = daemon_conn {
                            let msg = Message::CommandComplete(omnish_protocol::message::CommandComplete {
                                session_id: session_id.clone(),
                                record: record.clone(),
                            });
                            send_or_buffer(rpc, msg, &pending_buffer).await;
                        }
                    }
                    if !completed.is_empty() {
                        throttle.reset();
                    }
                }
                Err(_) => break,
            }
        }

        // Check if we should send a completion request (debounce)
        {
            let at_prompt = shell_input.at_prompt();
            let in_chat = interceptor.is_in_chat();
            let current = shell_input.input();

            // Clean up timed-out requests first
            let _cleaned = shell_completer.cleanup_timed_out_requests();

            if config.completion_enabled && at_prompt && !in_chat && !shell_input.in_isearch() && shell_input.cursor_at_end() && shell_completer.should_request(shell_input.sequence_id(), current) {
                let seq = shell_input.sequence_id();
                if let Some(ref rpc) = daemon_conn {
                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                    let msg = completion::ShellCompleter::build_request(
                        &session_id, current, seq, shell_cwd,
                    );
                    event_log::push(format!("completion request seq={seq} input={current:?}"));
                    shell_completer.mark_sent(seq, current);
                    let rpc_clone = rpc.clone();
                    let tx = completion_tx.clone();
                    tokio::spawn(async move {
                        if let Ok(Message::CompletionResponse(resp)) = rpc_clone.call(msg).await {
                            let _ = tx.send(resp).await;
                        }
                    });
                }
            }
        }

        // Check for completion responses (non-blocking)
        // Discard responses if user has entered chat mode or isearch mode.
        while let Ok(resp) = completion_rx.try_recv() {
            if interceptor.is_in_chat() {
                shell_completer.clear();
                continue;
            }

            // In isearch mode (Ctrl+R) — discard to avoid "cannot find keymap" error (issue #88)
            if shell_input.in_isearch() || shell_input.pending_rl_report() {
                event_log::push(format!("completion response seq={} discarded (isearch)", resp.sequence_id));
                continue;
            }

            event_log::push(format!("completion response seq={}", resp.sequence_id));
            // Store response in pending queue for processing after readline report
            let resp_seq = resp.sequence_id;
            pending_completion_responses.push(resp);

            // Trigger readline report if not already triggered.
            // Only inject the trigger if the user hasn't typed since the request
            // (sequence_id unchanged) to avoid "cannot find keymap" when bash
            // readline is in a transient state from active typing (issue #88).
            if !readline_triggered_for_completions {
                readline_triggered_for_completions = true;
                readline_trigger_time = Some(std::time::Instant::now());
                let cur_seq = shell_input.sequence_id();
                if osc133_hook_installed && shell_input.at_prompt()
                    && cur_seq == resp_seq
                {
                    shell_input.mark_pending_report();
                    event_log::push("readline request (completion)");
                    proxy.write_all(b"\x1b[13337~")?;
                } else if osc133_hook_installed && shell_input.at_prompt() {
                    event_log::push(format!(
                        "readline trigger skipped (seq mismatch: cur={} resp={})",
                        cur_seq, resp_seq
                    ));
                }
            }
        }

        // Timeout check for pending completion responses waiting for readline report
        if readline_triggered_for_completions && !pending_completion_responses.is_empty() {
            if let Some(trigger_time) = readline_trigger_time {
                if trigger_time.elapsed() > std::time::Duration::from_millis(500) {
                    // Timeout - process pending responses with current input
                    if shell_input.cursor_at_end() {
                        let current = shell_input.input();
                        for resp in pending_completion_responses.drain(..) {
                            if let Some(ghost) = shell_completer.on_response(&resp, current) {
                                let ghost_render = display::render_ghost_text(ghost);
                                nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                            }
                        }
                    } else {
                        pending_completion_responses.clear();
                    }
                    readline_triggered_for_completions = false;
                    readline_trigger_time = None;
                }
            }
        }

        // Auto-dismiss expired ghost text
        if shell_completer.is_ghost_expired(config.shell.ghost_timeout_ms) {
            // Send completion summary (ignored - ghost expired)
            if let Some(ref rpc) = daemon_conn {
                let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
            }
            shell_completer.clear();
            nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
        }

        // Check if PTY hung up
        if fds[1].revents & libc::POLLHUP != 0 {
            break;
        }
    }

    // Send session end
    if let Some(ref rpc) = daemon_conn {
        let msg = Message::SessionEnd(SessionEnd {
            session_id: session_id.clone(),
            timestamp_ms: timestamp_ms(),
            exit_code: None,
        });
        let _ = rpc.send(msg).await;
    }

    // Drop raw mode guard BEFORE process::exit, since exit() skips destructors
    drop(_raw_guard);

    let exit_code = proxy.wait().unwrap_or(1);
    std::process::exit(exit_code);
}

async fn connect_daemon(
    daemon_addr: &str,
    session_id: &str,
    parent_session_id: Option<String>,
    child_pid: u32,
    buffer: MessageBuffer,
) -> Option<RpcClient> {
    let socket_path = daemon_addr.to_string();
    let sid = session_id.to_string();
    let psid = parent_session_id.clone();

    // Load auth token
    let token_path = omnish_common::auth::default_token_path();
    let auth_token = match omnish_common::auth::load_token(&token_path) {
        Ok(t) => t,
        Err(e) => {
            notice(&format!("[omnish] Failed to load auth token: {}", e));
            notice("[omnish] Running in passthrough mode (no daemon)");
            return None;
        }
    };

    // Set up TLS connector for TCP mode
    let tls_connector = if socket_path.contains(':') {
        let tls_dir = omnish_transport::tls::default_tls_dir();
        let cert_path = tls_dir.join("cert.pem");
        match omnish_transport::tls::make_connector(&cert_path) {
            Ok(c) => Some(c),
            Err(e) => {
                notice(&format!("[omnish] Failed to set up TLS: {}", e));
                notice("[omnish] Running in passthrough mode (no daemon)");
                return None;
            }
        }
    } else {
        None
    };

    match RpcClient::connect_with_reconnect_notify(
        &socket_path,
        tls_connector,
        move |rpc| {
            let sid = sid.clone();
            let psid = psid.clone();
            let rpc = rpc.clone();
            let buffer = buffer.clone();
            let token = auth_token.clone();
            Box::pin(async move {
                // Authenticate first
                let auth_resp = rpc.call(Message::Auth(Auth {
                    token,
                    protocol_version: omnish_protocol::message::PROTOCOL_VERSION,
                })).await?;
                match &auth_resp {
                    Message::AuthFailed => anyhow::bail!("authentication failed"),
                    Message::AuthOk(ok) => {
                        if ok.protocol_version != omnish_protocol::message::PROTOCOL_VERSION {
                            notice(&format!(
                                "[omnish] Protocol mismatch \
                                 (client={}, daemon={}), waiting for daemon upgrade...",
                                omnish_protocol::message::PROTOCOL_VERSION,
                                ok.protocol_version
                            ));
                            anyhow::bail!("protocol mismatch (client={}, daemon={})",
                                omnish_protocol::message::PROTOCOL_VERSION,
                                ok.protocol_version);
                        }
                    }
                    // Old daemon that responds with Ack (no version info)
                    _ => {}
                }

                // Then register session
                let attrs = probe::default_session_probes(child_pid).collect_all();
                rpc.call(Message::SessionStart(SessionStart {
                    session_id: sid,
                    parent_session_id: psid,
                    timestamp_ms: timestamp_ms(),
                    attrs,
                })).await?;

                // Replay buffered messages after successful SessionStart
                let buffered: Vec<Message> = {
                    buffer.lock().await.drain(..).collect()
                };
                for msg in buffered {
                    if rpc.call(msg).await.is_err() {
                        break; // Connection broke again during replay
                    }
                }
                Ok(())
            })
        },
        Some(|| {
            notice("[omnish] reconnected to daemon");
        }),
    ).await {
        Ok(client) => {
            if client.is_connected().await {
                notice(&format!("[omnish] Connected to daemon (session: {})", &session_id[..8]));
            } else {
                notice("[omnish] Daemon not available, waiting for daemon to start...");
                notice(&format!("[omnish] Socket: {}", socket_path));
                notice("[omnish] To start: omnish-daemon");
            }
            Some(client)
        }
        Err(e) => {
            // This should not happen with our updated connect_with_reconnect,
            // but keep for backward compatibility
            notice(&format!("[omnish] Daemon not available ({}), running in passthrough mode", e));
            notice(&format!("[omnish] Socket: {}", socket_path));
            notice("[omnish] To start: omnish-daemon");
            None
        }
    }
}

pub(crate) fn get_terminal_size() -> Option<(u16, u16)> {
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
struct CursorTracker {
    col: u16,
    row: u16,
    state: ColTrackState,
    /// CSI parameter bytes buffer for parsing cursor movement sequences.
    csi_params: Vec<u8>,
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

impl CursorTracker {
    fn new() -> Self {
        Self::with_position(0, 0)
    }

    fn with_position(col: u16, row: u16) -> Self {
        Self {
            col,
            row,
            state: ColTrackState::Normal,
            csi_params: Vec::new(),
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
                        b'[' => {
                            self.csi_params.clear();
                            ColTrackState::Csi
                        }
                        b']' => ColTrackState::Osc,
                        _ => ColTrackState::Normal,
                    };
                }
                ColTrackState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.finish_csi(byte);
                        self.state = ColTrackState::Normal;
                    } else {
                        self.csi_params.push(byte);
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

    /// Parse CSI final byte and update row/col accordingly.
    fn finish_csi(&mut self, final_byte: u8) {
        match final_byte {
            // CUU — Cursor Up: \x1b[nA
            b'A' => {
                let n = self.parse_csi_param_1().max(1);
                self.row = self.row.saturating_sub(n);
            }
            // CUB — Cursor Back: \x1b[nD  (handled here for completeness)
            // CUD — Cursor Down: \x1b[nB
            b'B' => {
                let n = self.parse_csi_param_1().max(1);
                self.row = self.row.saturating_add(n);
            }
            // CUP / HVP — Cursor Position: \x1b[n;mH or \x1b[n;mf
            b'H' | b'f' => {
                let (r, c) = self.parse_csi_param_2();
                // CSI params are 1-based, convert to 0-based
                self.row = r.max(1) - 1;
                self.col = c.max(1) - 1;
            }
            // SD — Scroll Down: \x1b[nT — content moves down, cursor row unchanged
            // but conceptually row 0 content is now new
            // SU — Scroll Up: \x1b[nS — content moves up
            // IL — Insert Line: \x1b[nL — inserts lines at cursor, pushes down
            // These don't move the cursor position itself.
            _ => {}
        }
    }

    /// Parse a single numeric CSI parameter (default 0).
    fn parse_csi_param_1(&self) -> u16 {
        if self.csi_params.is_empty() {
            return 0;
        }
        std::str::from_utf8(&self.csi_params)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// Parse two semicolon-separated CSI parameters (default 1;1).
    fn parse_csi_param_2(&self) -> (u16, u16) {
        let s = std::str::from_utf8(&self.csi_params).unwrap_or("");
        let mut parts = s.splitn(2, ';');
        let a = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
        let b = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
        (a, b)
    }

    fn process_normal(&mut self, byte: u8) {
        match byte {
            0x1b => self.state = ColTrackState::Esc,
            b'\r' => self.col = 0,
            b'\n' => { self.row = self.row.saturating_add(1); }
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

/// Detects DSR (Device Status Report) responses in stdin: `\x1b[row;colR`.
/// Feeds bytes one at a time; returns Some((row, col)) when a complete
/// response is recognized, None otherwise. Consumed bytes are not forwarded.
struct DsrDetector {
    state: DsrState,
    buf: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq)]
enum DsrState {
    Normal,
    Esc,
    Csi,
}

impl DsrDetector {
    fn new() -> Self {
        Self { state: DsrState::Normal, buf: Vec::new() }
    }

    /// Feed a byte. Returns:
    /// - `Some(Some((row, col)))` — complete DSR response parsed, byte consumed
    /// - `Some(None)` — byte is part of an in-progress DSR response, consumed
    /// - `None` — byte is not part of a DSR response, should be forwarded
    fn feed(&mut self, byte: u8) -> Option<Option<(u16, u16)>> {
        match self.state {
            DsrState::Normal => {
                if byte == 0x1b {
                    self.state = DsrState::Esc;
                    self.buf.clear();
                    self.buf.push(byte);
                    Some(None) // consumed, pending
                } else {
                    None // not ours
                }
            }
            DsrState::Esc => {
                self.buf.push(byte);
                if byte == b'[' {
                    self.state = DsrState::Csi;
                    Some(None)
                } else {
                    // Not a CSI — abort, bytes need to be replayed
                    self.state = DsrState::Normal;
                    None // signal caller to replay buf
                }
            }
            DsrState::Csi => {
                self.buf.push(byte);
                if byte == b'R' {
                    // Complete: parse \x1b[row;colR
                    self.state = DsrState::Normal;
                    let params = &self.buf[2..self.buf.len() - 1]; // between '[' and 'R'
                    let parsed = self.parse_params(params);
                    self.buf.clear();
                    Some(parsed)
                } else if byte.is_ascii_digit() || byte == b';' {
                    Some(None) // still accumulating params
                } else {
                    // Not a DSR response (other CSI sequence)
                    self.state = DsrState::Normal;
                    None // signal caller to replay buf
                }
            }
        }
    }

    /// Parse "row;col" from param bytes.
    fn parse_params(&self, params: &[u8]) -> Option<(u16, u16)> {
        let s = std::str::from_utf8(params).ok()?;
        let mut parts = s.splitn(2, ';');
        let row: u16 = parts.next()?.parse().ok()?;
        let col: u16 = parts.next()?.parse().ok()?;
        Some((row, col))
    }

    /// Get buffered bytes (for replay when detection aborts mid-sequence).
    fn take_buf(&mut self) -> Vec<u8> {
        self.state = DsrState::Normal;
        std::mem::take(&mut self.buf)
    }

    /// Flush a pending bare ESC (state == Esc) after processing all bytes from
    /// a single read(). A bare ESC that isn't followed by `[` in the same read
    /// is almost certainly a user keypress, not the start of a DSR response.
    fn flush_bare_esc(&mut self) -> Option<Vec<u8>> {
        if self.state == DsrState::Esc {
            Some(self.take_buf())
        } else {
            None
        }
    }
}

/// Send a DSR (Device Status Report) query to the terminal.
fn query_cursor_position() {
    nix::unistd::write(std::io::stderr(), b"\x1b[6n").ok();
}

/// Build a tmux window-name escape sequence: `\x1bk<name>\x1b\\`.
/// Returns `None` when not inside tmux.
fn tmux_title(name: &str, in_tmux: bool) -> Option<String> {
    if !in_tmux {
        return None;
    }
    Some(format!("\x1bk{}\x1b\\", name))
}

/// Extract the command basename (first whitespace-delimited token) for tmux title.
fn command_basename(cmd: &str) -> &str {
    cmd.split_whitespace().next().unwrap_or(cmd)
}

/// Collect client debug state for /debug client command
#[allow(clippy::too_many_arguments)]
fn debug_client_state(
    shell_input: &shell_input::ShellInputTracker,
    interceptor: &interceptor::InputInterceptor,
    shell_completer: &completion::ShellCompleter,
    daemon_conn: &Option<RpcClient>,
    _osc133_detector: &omnish_tracker::osc133_detector::Osc133Detector,
    last_readline: &Option<String>,
    shell_pid: u32,
    col_tracker: &CursorTracker,
) -> String {
    let mut output = String::new();

    // Version info
    output.push_str(&format!("Version: omnish {}\n\n", omnish_common::VERSION));

    // Shell cwd
    output.push_str("Shell:\n");
    if let Some(cwd) = get_shell_cwd(shell_pid) {
        output.push_str(&format!("  cwd: {}\n", cwd));
    } else {
        output.push_str("  cwd: (unknown)\n");
    }
    output.push('\n');

    // Cursor position
    output.push_str(&format!("  cursor: row={}, col={}\n", col_tracker.row, col_tracker.col));
    output.push('\n');

    // Shell Input Tracker state
    output.push_str("Shell Input Tracker:\n");
    let (input, seq, at_prompt, pending_rl, esc_state) = shell_input.get_debug_info();
    output.push_str(&format!("  at_prompt: {}\n", at_prompt));
    output.push_str(&format!("  input: \"{}\"\n", input));
    output.push_str(&format!("  sequence_id: {}\n", seq));
    output.push_str(&format!("  pending_rl_report: {}\n", pending_rl));
    output.push_str(&format!("  esc_state: {}\n", esc_state));

    // Add ESC state description
    let esc_desc = match esc_state {
        0 => "normal",
        1 => "saw ESC",
        2 => "in CSI params",
        _ => "unknown",
    };
    output.push_str(&format!("  esc_state_desc: {}\n", esc_desc));

    // Add special mode detection based on pending_rl_report
    if pending_rl {
        output.push_str("  special_mode: readline report pending (Tab/Up/Down/Ctrl+R)\n");
    }

    // Show last readline content from bash if available
    if let Some(ref readline) = last_readline {
        output.push_str(&format!("  readline_report: \"{}\"\n", readline));

        // Compare tracked input vs readline content
        if input != *readline {
            output.push_str("  input_mismatch: true (tracked != readline)\n");
            output.push_str(&format!("  tracked_input: \"{}\"\n", input));
            output.push_str(&format!("  bash_readline: \"{}\"\n", readline));
        }
    }

    output.push('\n');

    // Interceptor state
    output.push_str("Input Interceptor:\n");
    output.push_str(&format!("  in_chat: {}\n", interceptor.is_in_chat()));
    output.push_str(&format!("  suppressed: {}\n", interceptor.is_suppressed()));
    output.push('\n');

    // Shell Completer state
    output.push_str("Shell Completer:\n");
    let (active_count, sent_seq, pending_seq, active_ids) = shell_completer.get_debug_state();
    output.push_str(&format!("  active_requests: {}\n", active_count));
    output.push_str(&format!("  sent_seq: {:?}\n", sent_seq));
    output.push_str(&format!("  pending_seq: {}\n", pending_seq));
    output.push_str(&format!("  active_request_ids: {:?}\n", active_ids));
    output.push_str(&format!("  should_request: {}\n",
        shell_completer.should_request(shell_input.sequence_id(), shell_input.input())));
    output.push_str(&format!("  ghost: {:?}\n", shell_completer.ghost()));
    output.push('\n');

    // Daemon connection state
    output.push_str("Daemon Connection:\n");
    match daemon_conn {
        Some(_rpc) => {
            output.push_str("  status: connected\n");
            // Note: is_connected() is async, so we can't call it here in a sync context
        }
        None => {
            output.push_str("  status: disconnected\n");
        }
    }
    output.push('\n');

    // OSC 133 detector state - we don't have a mode_active method, so skip it
    output.push_str("OSC 133 Detector:\n");
    output.push_str("  status: active (detecting OSC 133 sequences)\n");
    output
}

/// Check if forwarded bytes contain keys that modify readline state,
/// requiring a readline report from bash via the trigger sequence.
fn needs_readline_report(bytes: &[u8]) -> bool {
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            // Tab no longer triggers readline report here (issue #23)
            // Now triggered when completion response arrives, to avoid
            // interfering with bash completion list display
            // 0x09 => return true, // Tab - removed
            // Ctrl+R (0x12) is intentionally excluded: it enters isearch mode
            // which uses a different keymap where our bind -x doesn't exist,
            // causing "cannot find keymap for command". The pending_rl_report
            // mechanism serves as a fallback for Ctrl+R.
            0x1b if bytes.get(i + 1) == Some(&b'[') => {
                match bytes.get(i + 2) {
                    Some(b'A') | Some(b'B') => return true, // Up / Down
                    _ => {}
                }
            }
            _ => {}
        }
    }
    false
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

    fn is_active(&self) -> bool {
        self.active
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
                    if (s == b"\x1b[?1049h" || s == b"\x1b[?1049l"
                        || s == b"\x1b[?47h" || s == b"\x1b[?47l")
                        && self.active != entering
                    {
                        self.active = entering;
                        changed = true;
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

/// Parse a multi-index expression like "1,2,3,5" or "1,2-4,5" into sorted unique 1-based indices.
/// Returns None if the input is empty or contains invalid syntax.
pub(crate) fn parse_index_expr(s: &str) -> Option<Vec<usize>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut indices = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start: usize = start_s.trim().parse().ok()?;
            let end: usize = end_s.trim().parse().ok()?;
            if start == 0 || end == 0 || start > end {
                return None;
            }
            for i in start..=end {
                indices.push(i);
            }
        } else {
            let i: usize = part.parse().ok()?;
            if i == 0 {
                return None;
            }
            indices.push(i);
        }
    }
    indices.sort();
    indices.dedup();
    if indices.is_empty() { None } else { Some(indices) }
}

/// Parse a daemon command response as JSON. Returns None if not valid JSON.
pub(crate) fn parse_cmd_response(content: &str) -> Option<serde_json::Value> {
    serde_json::from_str(content).ok()
}

/// Get the display string from a parsed command response JSON.
pub(crate) fn cmd_display_str(json: &serde_json::Value) -> String {
    json.get("display")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Display a command result or write to file if redirected.
pub(crate) fn handle_command_result(content: &str, redirect: Option<&str>, shell_pid: u32) {
    if let Some(path) = redirect {
        // Resolve relative paths against shell's current working directory
        let resolved_path = if std::path::Path::new(path).is_relative() {
            match get_shell_cwd(shell_pid) {
                Some(shell_cwd) => std::path::Path::new(&shell_cwd).join(path),
                None => std::path::Path::new(path).to_path_buf(), // Fallback to relative to session cwd
            }
        } else {
            std::path::Path::new(path).to_path_buf()
        };

        match std::fs::write(&resolved_path, content) {
            Ok(_) => {
                let msg = display::render_response(&format!("Written to {}", resolved_path.display()));
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
}

/// Send a query to the daemon and display the result.
///
/// If `redirect` is Some, the response is written to the given file path instead of stdout.
/// If `show_thinking` is true, a thinking spinner is shown while waiting for the response
/// and a separator is appended after.
async fn send_daemon_query(
    query: &str,
    session_id: &str,
    rpc: &RpcClient,
    redirect: Option<&str>,
    show_thinking: bool,
    shell_pid: u32,
) {
    let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
    let mut status = LineStatus::new(cols as usize, 5);
    if show_thinking {
        nix::unistd::write(std::io::stdout(), status.show("(thinking...)").as_bytes()).ok();
    }

    let request_id = Uuid::new_v4().to_string()[..8].to_string();
    let request = Message::Request(Request {
        request_id: request_id.clone(),
        session_id: session_id.to_string(),
        query: query.to_string(),
        scope: RequestScope::AllSessions,
    });

    match rpc.call(request).await {
        Ok(Message::Response(resp)) if resp.request_id == request_id => {
            let display = if let Some(json) = parse_cmd_response(&resp.content) {
                cmd_display_str(&json)
            } else {
                resp.content.clone()
            };
            if show_thinking {
                std::fs::write("/tmp/omnish_last_response.txt", &display).ok();
                nix::unistd::write(std::io::stdout(), status.clear().as_bytes()).ok();
            }
            handle_command_result(&display, redirect, shell_pid);
            if show_thinking {
                let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                let separator = display::render_separator(cols);
                let sep_line = format!("{}\r\n", separator);
                nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
            }
        }
        _ => {
            nix::unistd::write(std::io::stdout(), status.clear().as_bytes()).ok();
            let err = display::render_error("Failed to receive response");
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        }
    }
}

/// Handle a /command in chat mode. Returns true if the command was handled.
pub(crate) async fn handle_slash_command(
    trimmed: &str,
    session_id: &str,
    rpc: &RpcClient,
    proxy: &PtyProxy,
    client_debug_fn: &dyn Fn() -> String,
    cursor_col: u16,
    cursor_row: u16,
) -> bool {
    // /update and /update auto are intercepted in DaemonQuery handling below
    // (they need process state: proxy fd/pid, and mutable auto_update_enabled)
    // /update auto is intercepted at the call site (needs mutable auto_update_enabled)

    match command::dispatch(trimmed) {
        command::ChatAction::Command { result, redirect, limit } => {
            let display_result = if let Some(ref l) = limit {
                command::apply_limit(&result, l)
            } else {
                result
            };
            if let Some(path) = redirect.as_deref() {
                handle_command_result(&display_result, Some(path), proxy.child_pid() as u32);
            } else {
                // Command output is plain text — skip markdown rendering
                // Single-line output: no leading blank line; multi-line: add one for readability
                let is_multiline = display_result.contains('\n');
                let prefix = if is_multiline { "\r\n" } else { "" };
                let output = format!("{}{}\r\n", prefix, display_result.replace('\n', "\r\n"));
                nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
            }
            true
        }
        command::ChatAction::DaemonQuery { query, redirect, limit } => {
            // /debug client is intercepted client-side (needs local state)
            if query == "__cmd:client_debug" {
                let result = client_debug_fn();
                let display_result = if let Some(ref l) = limit {
                    command::apply_limit(&result, l)
                } else {
                    result
                };
                if let Some(path) = redirect.as_deref() {
                    handle_command_result(&display_result, Some(path), proxy.child_pid() as u32);
                } else {
                    // Plain text output — skip markdown rendering to preserve blank lines
                    let output = format!("\r\n{}\r\n", display_result.replace('\n', "\r\n"));
                    nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                }
                return true;
            } else if query == "__cmd:update" {
                exec_update(proxy, session_id, cursor_col, cursor_row);
                return true; // Only reached if exec failed
            }
            if let Some(path) = redirect.as_deref() {
                send_daemon_query(&query, session_id, rpc, Some(path), false, proxy.child_pid() as u32).await;
            } else {
                let request_id = Uuid::new_v4().to_string()[..8].to_string();
                let request = Message::Request(Request {
                    request_id: request_id.clone(),
                    session_id: session_id.to_string(),
                    query,
                    scope: RequestScope::AllSessions,
                });
                match rpc.call(request).await {
                    Ok(Message::Response(resp)) if resp.request_id == request_id => {
                        let display = if let Some(json) = parse_cmd_response(&resp.content) {
                            cmd_display_str(&json)
                        } else {
                            resp.content
                        };
                        let display = if let Some(ref l) = limit {
                            command::apply_limit(&display, l)
                        } else {
                            display
                        };
                        // Command output is plain text — skip markdown rendering
                        let output = format!("\r\n{}\r\n", display.replace('\n', "\r\n"));
                        nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                    }
                    _ => {
                        let err = display::render_error("Failed to receive response");
                        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                    }
                }
            }
            true
        }
        command::ChatAction::LlmQuery(_) => false,
    }
}

// Chat loop, input handling, and related helpers are in chat_session.rs.

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
        assert!(!d.active);
    }

    #[test]
    fn test_alt_screen_integration_with_interceptor() {
        use interceptor::AlwaysIntercept;

        let mut interceptor = InputInterceptor::new(":", "::", Box::new(AlwaysIntercept));
        let mut detector = AltScreenDetector::new();

        // Normal mode: ":" matches prefix → Buffering (awaiting timeout)
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.expire_prefix(), Some(InterceptAction::Chat(String::new())));

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

        // Back to normal: ":" should intercept again → Buffering
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Buffering(vec![b':']));
        assert_eq!(interceptor.expire_prefix(), Some(InterceptAction::Chat(String::new())));
    }

    // --- Message buffer tests ---

    #[test]
    fn test_should_buffer_io_data() {
        let msg = Message::IoData(IoData {
            session_id: "s1".to_string(),
            direction: IoDirection::Input,
            timestamp_ms: 1000,
            data: b"ls".to_vec(),
        });
        assert!(should_buffer(&msg));
    }

    #[test]
    fn test_should_buffer_command_complete() {
        let msg = Message::CommandComplete(omnish_protocol::message::CommandComplete {
            session_id: "s1".to_string(),
            record: omnish_store::command::CommandRecord {
                command_id: "c1".to_string(),
                session_id: "s1".to_string(),
                command_line: Some("ls".to_string()),
                cwd: None,
                started_at: 1000,
                ended_at: Some(2000),
                output_summary: String::new(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        });
        assert!(should_buffer(&msg));
    }

    #[test]
    fn test_should_not_buffer_session_start() {
        let msg = Message::SessionStart(SessionStart {
            session_id: "s1".to_string(),
            parent_session_id: None,
            timestamp_ms: 1000,
            attrs: HashMap::new(),
        });
        assert!(!should_buffer(&msg));
    }

    #[test]
    fn test_should_not_buffer_other_types() {
        assert!(!should_buffer(&Message::Ack));
        assert!(!should_buffer(&Message::SessionEnd(SessionEnd {
            session_id: "s1".to_string(),
            timestamp_ms: 1000,
            exit_code: None,
        })));
        assert!(!should_buffer(&Message::Request(Request {
            request_id: "r1".to_string(),
            session_id: "s1".to_string(),
            query: "test".to_string(),
            scope: RequestScope::CurrentSession,
        })));
    }

    #[tokio::test]
    async fn test_buffer_cap_drops_oldest() {
        let buffer: MessageBuffer = Arc::new(Mutex::new(VecDeque::new()));
        {
            let mut buf = buffer.lock().await;
            for i in 0..MAX_BUFFER_SIZE + 1 {
                let msg = Message::IoData(IoData {
                    session_id: "s1".to_string(),
                    direction: IoDirection::Output,
                    timestamp_ms: i as u64,
                    data: vec![i as u8],
                });
                if buf.len() >= MAX_BUFFER_SIZE {
                    buf.pop_front();
                }
                buf.push_back(msg);
            }
            assert_eq!(buf.len(), MAX_BUFFER_SIZE);
            // Oldest (timestamp 0) was dropped; front should be timestamp 1
            if let Some(Message::IoData(io)) = buf.front() {
                assert_eq!(io.timestamp_ms, 1);
            } else {
                panic!("expected IoData at front of buffer");
            }
        }
    }

    // --- CursorTracker tests ---

    #[test]
    fn test_col_tracker_ascii() {
        let mut t = CursorTracker::new();
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
        let mut t = CursorTracker::new();
        // Color escape sequences should not advance column
        t.feed(b"\x1b[32mgreen\x1b[0m");
        assert_eq!(t.col, 5); // only "green" counted
    }

    #[test]
    fn test_col_tracker_skips_osc() {
        let mut t = CursorTracker::new();
        // OSC title sequence (invisible) then prompt
        t.feed(b"\x1b]0;my title\x07$ ");
        assert_eq!(t.col, 2); // only "$ " counted
    }

    #[test]
    fn test_col_tracker_typical_prompt() {
        let mut t = CursorTracker::new();
        // Typical colored prompt: \r\n\x1b[32muser@host\x1b[0m:\x1b[34m~\x1b[0m$
        t.feed(b"\r\n\x1b[32muser@host\x1b[0m:\x1b[34m~\x1b[0m$ ");
        // "user@host" (9) + ":" (1) + "~" (1) + "$ " (2) = 13
        assert_eq!(t.col, 13);
    }

    #[test]
    fn test_col_tracker_cjk_wide_chars() {
        let mut t = CursorTracker::new();
        // Chinese characters are fullwidth — each occupies 2 columns
        t.feed("你好".as_bytes());
        assert_eq!(t.col, 4); // 2 chars × 2 columns each

        // Mixed: CJK + ASCII
        t = CursorTracker::new();
        t.feed("用户@主机:~$ ".as_bytes());
        // "用" (2) + "户" (2) + "@" (1) + "主" (2) + "机" (2) + ":" (1) + "~" (1) + "$ " (2) = 13
        assert_eq!(t.col, 13);
    }

    #[test]
    fn test_col_tracker_cjk_with_colors() {
        let mut t = CursorTracker::new();
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
        let mut t = CursorTracker::new();
        // ❯ (U+276F) is narrow — width 1
        t.feed("❯ ".as_bytes());
        assert_eq!(t.col, 2); // ❯ (1) + space (1)

        // 🚀 (U+1F680) is a wide emoji — width 2
        t = CursorTracker::new();
        t.feed("🚀x".as_bytes());
        assert_eq!(t.col, 3); // 🚀 (2) + x (1)
    }

    // --- CursorRowTracker tests ---

    #[test]
    fn test_row_tracker_newline() {
        let mut t = CursorTracker::new();
        assert_eq!(t.row, 0);
        t.feed(b"hello\n");
        assert_eq!(t.row, 1);
        t.feed(b"line2\nline3\n");
        assert_eq!(t.row, 3);
    }

    #[test]
    fn test_row_tracker_cursor_home() {
        let mut t = CursorTracker::new();
        t.feed(b"line1\r\nline2\r\nline3");
        assert_eq!(t.row, 2);
        assert_eq!(t.col, 5);
        // \x1b[H — cursor to (0,0)
        t.feed(b"\x1b[H");
        assert_eq!(t.row, 0);
        assert_eq!(t.col, 0);
    }

    #[test]
    fn test_row_tracker_cup_with_params() {
        let mut t = CursorTracker::new();
        // \x1b[5;10H — cursor to row 5, col 10 (1-based → 4, 9 zero-based)
        t.feed(b"\x1b[5;10H");
        assert_eq!(t.row, 4);
        assert_eq!(t.col, 9);
    }

    #[test]
    fn test_row_tracker_cursor_up_down() {
        let mut t = CursorTracker::new();
        t.feed(b"\n\n\n\n\n"); // row = 5
        assert_eq!(t.row, 5);
        // \x1b[2A — cursor up 2
        t.feed(b"\x1b[2A");
        assert_eq!(t.row, 3);
        // \x1b[B — cursor down 1 (no param = 1)
        t.feed(b"\x1b[B");
        assert_eq!(t.row, 4);
    }

    #[test]
    fn test_row_tracker_cursor_up_saturates() {
        let mut t = CursorTracker::new();
        t.feed(b"\n"); // row = 1
        // Move up 10 — should saturate to 0
        t.feed(b"\x1b[10A");
        assert_eq!(t.row, 0);
    }

    #[test]
    fn test_row_tracker_clear_screen() {
        let mut t = CursorTracker::new();
        t.feed(b"line1\nline2\nline3");
        assert_eq!(t.row, 2);
        // clear command outputs \x1b[H\x1b[2J
        t.feed(b"\x1b[H\x1b[2J");
        assert_eq!(t.row, 0);
        assert_eq!(t.col, 0);
    }

    // --- DSR detector tests ---

    #[test]
    fn test_dsr_complete_response() {
        let mut d = DsrDetector::new();
        // Feed \x1b[24;13R
        assert_eq!(d.feed(0x1b), Some(None));   // ESC consumed
        assert_eq!(d.feed(b'['), Some(None));    // [ consumed
        assert_eq!(d.feed(b'2'), Some(None));    // digit
        assert_eq!(d.feed(b'4'), Some(None));    // digit
        assert_eq!(d.feed(b';'), Some(None));    // semicolon
        assert_eq!(d.feed(b'1'), Some(None));    // digit
        assert_eq!(d.feed(b'3'), Some(None));    // digit
        assert_eq!(d.feed(b'R'), Some(Some((24, 13)))); // complete
    }

    #[test]
    fn test_dsr_row_1_col_1() {
        let mut d = DsrDetector::new();
        for &b in b"\x1b[1;1" {
            assert_eq!(d.feed(b), Some(None));
        }
        assert_eq!(d.feed(b'R'), Some(Some((1, 1))));
    }

    #[test]
    fn test_dsr_normal_input_passes_through() {
        let mut d = DsrDetector::new();
        assert_eq!(d.feed(b'a'), None);
        assert_eq!(d.feed(b'\n'), None);
        assert_eq!(d.feed(b':'), None);
    }

    #[test]
    fn test_dsr_non_csi_esc_aborts() {
        let mut d = DsrDetector::new();
        assert_eq!(d.feed(0x1b), Some(None)); // ESC consumed
        assert_eq!(d.feed(b'O'), None);        // not '[', abort — replay
        assert!(!d.buf.is_empty());
        let replay = d.take_buf();
        assert_eq!(replay, vec![0x1b, b'O']);
    }

    #[test]
    fn test_dsr_non_r_final_aborts() {
        let mut d = DsrDetector::new();
        // \x1b[2A — cursor up, not a DSR response
        assert_eq!(d.feed(0x1b), Some(None));
        assert_eq!(d.feed(b'['), Some(None));
        assert_eq!(d.feed(b'2'), Some(None));
        assert_eq!(d.feed(b'A'), None); // final byte but not 'R' — abort
        let replay = d.take_buf();
        assert_eq!(replay, vec![0x1b, b'[', b'2', b'A']);
    }

    // --- tmux title tests ---

    #[test]
    fn test_tmux_title_in_tmux() {
        let result = tmux_title("omnish", true);
        assert_eq!(result, Some("\x1bkomnish\x1b\\".to_string()));
    }

    #[test]
    fn test_tmux_title_not_in_tmux() {
        assert_eq!(tmux_title("omnish", false), None);
    }

    #[test]
    fn test_tmux_title_command_name() {
        let result = tmux_title("vim", true);
        assert_eq!(result, Some("\x1bkvim\x1b\\".to_string()));
    }

    #[test]
    fn test_command_basename_simple() {
        assert_eq!(command_basename("vim"), "vim");
    }

    #[test]
    fn test_command_basename_with_args() {
        assert_eq!(command_basename("git status"), "git");
    }

    #[test]
    fn test_command_basename_with_path() {
        assert_eq!(command_basename("/usr/bin/vim file.txt"), "/usr/bin/vim");
    }

    #[test]
    fn test_command_basename_empty() {
        assert_eq!(command_basename(""), "");
    }

    #[test]
    fn test_history_navigation() {
        use std::collections::VecDeque;

        let mut history = VecDeque::new();
        history.push_back("command1".to_string());
        history.push_back("command2".to_string());

        let mut idx = None;

        // Simulate up arrow - should go to command2 (most recent)
        idx = match idx {
            Some(i) if i > 0 => Some(i - 1),
            Some(_) => Some(0),
            None => Some(history.len() - 1),
        };
        assert_eq!(idx, Some(1));

        // Another up arrow - should go to command1
        idx = match idx {
            Some(i) if i > 0 => Some(i - 1),
            Some(_) => Some(0),
            None => Some(history.len() - 1),
        };
        assert_eq!(idx, Some(0));

        // Down arrow - should go back to command2
        idx = match idx {
            Some(i) if i < history.len() - 1 => Some(i + 1),
            Some(_) => {
                // Going past most recent
                None
            },
            None => None,
        };
        assert_eq!(idx, Some(1));
    }

    // --- parse_index_expr tests ---

    #[test]
    fn test_parse_index_expr_single() {
        assert_eq!(parse_index_expr("3"), Some(vec![3]));
    }

    #[test]
    fn test_parse_index_expr_comma_separated() {
        assert_eq!(parse_index_expr("1,2,3,5"), Some(vec![1, 2, 3, 5]));
    }

    #[test]
    fn test_parse_index_expr_range() {
        assert_eq!(parse_index_expr("2-4"), Some(vec![2, 3, 4]));
    }

    #[test]
    fn test_parse_index_expr_mixed() {
        assert_eq!(parse_index_expr("1,3-5,8"), Some(vec![1, 3, 4, 5, 8]));
    }

    #[test]
    fn test_parse_index_expr_dedup() {
        assert_eq!(parse_index_expr("1,2,2,3"), Some(vec![1, 2, 3]));
    }

    #[test]
    fn test_parse_index_expr_spaces() {
        assert_eq!(parse_index_expr(" 1 , 3 - 5 "), Some(vec![1, 3, 4, 5]));
    }

    #[test]
    fn test_parse_index_expr_invalid() {
        assert_eq!(parse_index_expr(""), None);
        assert_eq!(parse_index_expr("abc"), None);
        assert_eq!(parse_index_expr("0"), None);
        assert_eq!(parse_index_expr("5-3"), None);
        assert_eq!(parse_index_expr(","), None);
    }
}
