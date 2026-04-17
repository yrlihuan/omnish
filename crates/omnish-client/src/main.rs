// crates/omnish-client/src/main.rs
mod chat_session;
mod client_plugin;
mod command;
mod completion;
pub mod event_log;
mod ghost_complete;
mod display;
mod i18n;
mod interceptor;
mod markdown;
mod probe;
mod shell_hook;
mod shell_input;
mod throttle;
mod util;
mod onboarding;
mod widgets;

use anyhow::{Context, Result};
use omnish_common::config::{load_client_config, ClientSandboxConfig};
use interceptor::{InputInterceptor, InterceptAction, TimeGapGuard};
use widgets::line_status::LineStatus;
use omnish_protocol::message::*;
use omnish_pty::proxy::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;
use omnish_transport::rpc_client::RpcClient;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::{Arc, RwLock};
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
        // $SHELL points to omnish - fall back to config, then common defaults
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
    last_thread_id: Option<String>,
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
    // cursor-col, cursor-row, last-thread-id passed via env vars
    let cursor_col = std::env::var("OMNISH_CURSOR_COL").ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    let cursor_row = std::env::var("OMNISH_CURSOR_ROW").ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);
    let last_thread_id = std::env::var("OMNISH_LAST_THREAD_ID").ok()
        .filter(|s| !s.is_empty());
    Some(ResumeArgs { master_fd: fd, child_pid: pid, session_id: sid, cursor_col, cursor_row, last_thread_id })
}

mod notice_queue {
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::sync::Mutex;

    static DEFERRED: AtomicBool = AtomicBool::new(false);
    static ALT_SCREEN: AtomicBool = AtomicBool::new(false);
    static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// Current cursor row, updated by CursorTracker.
    static CURSOR_ROW: AtomicU16 = AtomicU16::new(0);

    /// Update the tracked cursor row (called from CursorTracker after feed).
    pub fn set_cursor_row(row: u16) {
        CURSOR_ROW.store(row, Ordering::Relaxed);
    }

    /// Set alternate screen state. When active, notices are queued.
    pub fn set_alt_screen(active: bool) {
        ALT_SCREEN.store(active, Ordering::Relaxed);
        if !active {
            // Leaving alt screen - flush queued notices
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
    }

    /// Queue a notice. If deferred or alt-screen mode is on, store it; otherwise display immediately.
    pub fn push(msg: &str) {
        if DEFERRED.load(Ordering::Relaxed) || ALT_SCREEN.load(Ordering::Relaxed) {
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
        if ALT_SCREEN.load(Ordering::Relaxed) {
            return; // Still in alt screen, keep queued
        }
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

fn exec_update(proxy: &PtyProxy, session_id: &str, cursor_col: u16, cursor_row: u16, last_thread_id: Option<&str>) {
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
        tracing::debug!("exec_update: binary unchanged ({})", omnish_common::VERSION);
        return;
    }

    notice(&format!("[omnish] Updating: {} -> {}", running_version, disk_version));

    // Clear FD_CLOEXEC on the PTY master fd so it survives exec
    let master_fd = proxy.master_raw_fd();
    unsafe {
        let flags = libc::fcntl(master_fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(master_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }

    // Pass transient state via env vars (survives exec, visible in /proc/<pid>/environ)
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    std::env::set_var("OMNISH_STARTED", &started);
    std::env::set_var("OMNISH_CURSOR_COL", cursor_col.to_string());
    std::env::set_var("OMNISH_CURSOR_ROW", cursor_row.to_string());
    if let Some(tid) = last_thread_id {
        std::env::set_var("OMNISH_LAST_THREAD_ID", tid);
    } else {
        std::env::remove_var("OMNISH_LAST_THREAD_ID");
    }

    // Build args for the new process (fd, pid, session-id stay as CLI args)
    let exe_cstr = std::ffi::CString::new(current_exe.to_string_lossy().as_bytes()).unwrap();
    let args = [
        exe_cstr.clone(),
        std::ffi::CString::new("--resume").unwrap(),
        std::ffi::CString::new(format!("--fd={}", master_fd)).unwrap(),
        std::ffi::CString::new(format!("--pid={}", proxy.child_pid())).unwrap(),
        std::ffi::CString::new(format!("--session-id={}", session_id)).unwrap(),
    ];

    // execvp replaces this process - only returns on error
    let _ = nix::unistd::execvp(&exe_cstr, &args);
    notice(&format!("[omnish] exec failed: {}", std::io::Error::last_os_error()));
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    omnish_common::update::compare_versions(a, b)
}

async fn download_and_extract_update(
    rpc: &RpcClient,
    os: &str,
    arch: &str,
    version: &str,
    hostname: &str,
) -> anyhow::Result<()> {
    let mut rx = rpc.call_stream(Message::UpdateRequest {
        os: os.to_string(),
        arch: arch.to_string(),
        version: version.to_string(),
        hostname: hostname.to_string(),
    }).await.context("call_stream UpdateRequest")?;

    let omnish_dir = omnish_common::config::omnish_dir();

    // Save to updates/{os}-{arch}/ (same layout as daemon cache)
    let updates_dir = omnish_dir.join("updates").join(format!("{}-{}", os, arch));
    std::fs::create_dir_all(&updates_dir).context("create updates_dir")?;
    let pkg_file = updates_dir.join(format!("omnish-{}-{}-{}.tar.gz", version, os, arch));
    let tmp_download = updates_dir.join(format!(".tmp-omnish-{}-{}-{}-{}.tar.gz", version, os, arch, std::process::id()));

    let mut expected_checksum = String::new();
    let mut chunk_count = 0u32;
    let mut total_bytes = 0u64;
    let mut got_done = false;
    // Defer file creation until first valid chunk to avoid creating/deleting
    // tmp files when the daemon immediately rejects with "transfer in progress".
    let mut file: Option<tokio::fs::File> = None;

    use tokio::io::AsyncWriteExt;
    while let Some(msg) = rx.recv().await {
        match msg {
            Message::UpdateChunk { seq, total_size: _, checksum, data, done, error } => {
                if let Some(err) = error {
                    if let Some(f) = file.take() {
                        drop(f);
                        let _ = tokio::fs::remove_file(&tmp_download).await;
                    }
                    anyhow::bail!("server error: {}", err);
                }
                if seq == 0 {
                    expected_checksum = checksum;
                }
                if !data.is_empty() {
                    if file.is_none() {
                        file = Some(tokio::fs::File::create(&tmp_download).await
                            .with_context(|| format!("create tmp file {}", tmp_download.display()))?);
                    }
                    total_bytes += data.len() as u64;
                    file.as_mut().unwrap().write_all(&data).await.context("write chunk to tmp file")?;
                }
                chunk_count += 1;
                if done {
                    got_done = true;
                    break;
                }
            }
            other => {
                tracing::warn!("download stream: unexpected message {:?}, aborting after {} chunks",
                    std::mem::discriminant(&other), chunk_count);
                break;
            }
        }
    }
    if let Some(mut f) = file.take() {
        f.flush().await.context("flush tmp file")?;
        drop(f);
    }

    if !got_done {
        let _ = tokio::fs::remove_file(&tmp_download).await;
        anyhow::bail!("download stream ended prematurely after {} chunks ({} bytes), no done marker",
            chunk_count, total_bytes);
    }
    tracing::info!("download complete: {} chunks, {} bytes", chunk_count, total_bytes);

    // Verify checksum by re-reading the written file
    if !expected_checksum.is_empty() {
        let tmp_clone = tmp_download.clone();
        let actual = tokio::task::spawn_blocking(move || {
            omnish_common::update::checksum(&tmp_clone)
        }).await.context("checksum spawn_blocking join")??;

        if actual != expected_checksum {
            let _ = tokio::fs::remove_file(&tmp_download).await;
            anyhow::bail!("checksum mismatch: expected={}, actual={}", expected_checksum, actual);
        }
    }

    // Move temp file to final location in updates cache
    tokio::fs::rename(&tmp_download, &pkg_file).await
        .with_context(|| format!("rename {} -> {}", tmp_download.display(), pkg_file.display()))?;
    tracing::info!("cached update package: {}", pkg_file.display());

    // Extract and run install.sh --upgrade --client-only
    let pkg_for_extract = pkg_file.clone();
    let version_for_extract = version.to_string();
    tokio::task::spawn_blocking(move || {
        omnish_common::update::extract_and_run_installer(&pkg_for_extract, &version_for_extract, true)
    })
        .await
        .context("extract spawn_blocking join")?
        .context("extract_and_run_installer")?;

    // Prune old packages, keeping the latest 3
    let os_owned = os.to_string();
    let arch_owned = arch.to_string();
    let updates_dir_clone = updates_dir.clone();
    tokio::task::spawn_blocking(move || {
        omnish_common::update::prune_packages(&updates_dir_clone, &os_owned, &arch_owned, omnish_common::update::MAX_CACHED_PACKAGES);
    })
        .await
        .context("prune spawn_blocking join")?;

    tracing::info!("update {} installed, mtime polling will trigger restart", version);
    Ok(())
}

// extract_and_run_installer moved to omnish_common::update

#[tokio::main(worker_threads = 4)]
async fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("omnish {}", omnish_common::VERSION);
        return Ok(());
    }

    // Initialize file-based tracing for debugging (does not write to stderr/stdout to avoid PTY interference)
    {
        let log_path = omnish_common::config::omnish_dir().join("client.log");
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
            use tracing_subscriber::layer::SubscriberExt;
            use tracing_subscriber::util::SubscriberInitExt;
            let filter = tracing_subscriber::EnvFilter::new("debug");
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init();
        }
    }

    let config = load_client_config().unwrap_or_default();
    let lang = std::env::var("OMNISH_LANG").unwrap_or_else(|_| config.shell.language.clone());
    i18n::init(&lang);
    let onboarded = Arc::new(AtomicBool::new(config.onboarded));
    let resume_args = parse_resume_args();

    // If stdin is not a terminal (e.g. rsync over SSH, piped commands),
    // exec the underlying shell directly - omnish requires a PTY.
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

    // Resolve shell and hook args early so they're available for respawn.
    let shell = resolve_shell(&config.shell.command);

    // Install shell-specific OSC 133 hook
    let osc133_rcfile = shell_hook::install_bash_hook(&shell);
    let osc133_zdotdir = shell_hook::install_zsh_hook(&shell);
    let osc133_hook_installed = osc133_rcfile.is_some() || osc133_zdotdir.is_some();

    let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
        vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
    } else {
        vec![]
    };
    let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();

    let (session_id, mut proxy, osc133_hook_installed) = if let Some(ref resume) = resume_args {
        // Set cursor row early so resume notice uses correct rendering mode
        notice_queue::set_cursor_row(resume.cursor_row);
        // Resume mode: reconstruct PtyProxy from passed fd/pid
        let proxy = unsafe { PtyProxy::from_raw_fd(resume.master_fd, resume.child_pid) };
        notice(&format!("[omnish] Resumed (pid={}, fd={})", resume.child_pid, resume.master_fd));
        (resume.session_id.clone(), proxy, osc133_hook_installed)
    } else {
        // Normal startup: spawn a new shell
        let session_id = Uuid::new_v4().to_string()[..8].to_string();

        let mut child_env = HashMap::new();
        child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
        child_env.insert("SHELL".to_string(), shell.clone());

        if let Some(ref zdotdir) = osc133_zdotdir {
            // Preserve original ZDOTDIR so the hook can source the user's .zshrc
            if let Ok(orig) = std::env::var("ZDOTDIR") {
                child_env.insert("OMNISH_ORIG_ZDOTDIR".to_string(), orig);
            }
            child_env.insert("ZDOTDIR".to_string(), zdotdir.to_string_lossy().to_string());
        }

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
    let update_needed = Arc::new(AtomicBool::new(false));
    let daemon_conn = connect_daemon(&daemon_addr, &session_id, parent_session_id, proxy.child_pid() as u32, pending_buffer.clone(), update_needed.clone()).await;

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
                        let process_name = if process_name.is_empty() { "omnish" } else { process_name };
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
    let mut master_fd = proxy.master_raw_fd();
    setup_sigwinch(master_fd);

    // Main I/O loop using poll
    let mut input_buf = [0u8; 4096];
    let mut output_buf = [0u8; 4096];
    let guard = TimeGapGuard::new(std::time::Duration::from_millis(config.shell.intercept_gap_ms));
    let mut interceptor = InputInterceptor::new(&config.shell.command_prefix, &config.shell.resume_prefix, Box::new(guard), config.shell.developer_mode);
    let mut prefix_bytes: Vec<u8> = config.shell.command_prefix.as_bytes().to_vec();
    let mut completion_enabled = config.shell.completion_enabled;
    let mut ghost_timeout_ms = config.shell.ghost_timeout_ms;
    // Client-local sandbox state (enabled + preferred backend), shared with
    // chat session so menu edits can update it and flow into subsequent chat
    // sessions. Writes persist to client.toml via save_client_local_config.
    let sandbox_state: Arc<RwLock<ClientSandboxConfig>> = Arc::new(RwLock::new(config.sandbox.clone()));
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
    // Deferred ghost text render - rendered after next PTY display_data write
    // so bash's readline redraw (after bind-x hook) doesn't overwrite it.
    let mut deferred_ghost: Option<String> = None;
    // Display width of the deferred ghost suffix, for wrap detection at flush time
    let mut deferred_ghost_width: usize = 0;
    // Whether the currently-rendered ghost text wrapped to the next terminal line.
    // Set at render time, used at clear time to decide whether to erase the next line.
    let mut ghost_wrapped = false;
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
    let mut last_thread_id: Option<String> = resume_args.as_ref().and_then(|r| r.last_thread_id.clone());

    // Auto-update state
    let mut exe_mtime = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let s = p.to_string_lossy().to_string();
            let clean = s.strip_suffix(" (deleted)").map(std::path::PathBuf::from).unwrap_or(p);
            std::fs::metadata(&clean).ok()?.modified().ok()
        });
    let mut last_update_check = std::time::Instant::now();
    const AUTO_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
    const AUTO_UPDATE_IDLE: std::time::Duration = std::time::Duration::from_secs(60);
    let mut last_keystroke = std::time::Instant::now();
    let update_in_progress = Arc::new(AtomicBool::new(false));

    // Landlock sandbox state
    let mut locked = false;

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

        // Mtime restart check (every 60s, only when idle at prompt)
        // WARNING: The UpdateCheck block below must NOT be gated on at_prompt/idle/alt_screen.
        // Those conditions only guard the mtime restart. If UpdateCheck is blocked by them,
        // clients that are busy (running commands, in vim, etc.) will never download updates,
        // creating a chicken-and-egg problem where old clients can't get the new code.
        if last_update_check.elapsed() >= AUTO_UPDATE_INTERVAL
            && !interceptor.is_in_chat()
        {
            last_update_check = std::time::Instant::now();

            // Restart with new binary: only when user is idle at prompt
            if shell_input.at_prompt()
                && last_keystroke.elapsed() >= AUTO_UPDATE_IDLE
                && !alt_screen_detector.is_active()
            {
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
                            exec_update(&proxy, &session_id, col_tracker.col, col_tracker.row, last_thread_id.as_deref());
                            // exec_update only returns on error - reset timer
                        }
                        exe_mtime = current_mtime;
                    }
                }
            }

            // Periodic UpdateCheck: download update when daemon has a newer version
            if update_needed.load(Ordering::Relaxed)
                && !update_in_progress.load(Ordering::Relaxed)
            {
                if let Some(ref rpc) = daemon_conn {
                    let os = std::env::consts::OS.to_string();
                    let arch = std::env::consts::ARCH.to_string();
                    let ver = omnish_common::VERSION.to_string();
                    let hostname = nix::unistd::gethostname()
                        .ok()
                        .and_then(|h| h.into_string().ok())
                        .unwrap_or_default();
                    event_log::push(format!("update_check: v={} connected={}", ver, rpc.is_connected().await));
                    let check_result = rpc.call(Message::UpdateCheck {
                        os: os.clone(), arch: arch.clone(), current_version: ver,
                        hostname: hostname.clone(),
                    }).await;
                    match &check_result {
                        Ok(Message::UpdateInfo { latest_version, checksum: remote_checksum, available: true }) => {
                            // Check if local cache already has this version with matching checksum
                            let need_download = if let Some((cached_ver, cached_path)) =
                                omnish_common::update::local_cached_package(&os, &arch)
                            {
                                if compare_versions(latest_version, &cached_ver) == std::cmp::Ordering::Equal {
                                    let local_checksum = omnish_common::update::checksum(&cached_path)
                                        .unwrap_or_default();
                                    if local_checksum == *remote_checksum && !remote_checksum.is_empty() {
                                        event_log::push(format!("update_check: v={} cached, checksum match", latest_version));
                                        false
                                    } else {
                                        event_log::push(format!("update_check: v={} cached, checksum mismatch", latest_version));
                                        true
                                    }
                                } else {
                                    true
                                }
                            } else {
                                true
                            };

                            event_log::push(format!("update_check: available v={} download={}", latest_version, need_download));
                            update_in_progress.store(true, Ordering::Relaxed);
                            let latest_version = latest_version.clone();
                            let rpc = rpc.clone();
                            let uip = Arc::clone(&update_in_progress);
                            let un = Arc::clone(&update_needed);
                            if need_download {
                                tokio::spawn(async move {
                                    event_log::push(format!("update_download: start v={}", latest_version));
                                    if let Err(e) = download_and_extract_update(
                                        &rpc, &os, &arch, &latest_version, &hostname,
                                    ).await {
                                        event_log::push(format!("update_download: failed {}", e));
                                        tracing::warn!("update download failed: {}", e);
                                    } else {
                                        event_log::push(format!("update_download: done v={}", latest_version));
                                        un.store(false, Ordering::Relaxed);
                                    }
                                    uip.store(false, Ordering::Relaxed);
                                });
                            } else {
                                let os_clone = os.clone();
                                let arch_clone = arch.clone();
                                tokio::spawn(async move {
                                    event_log::push(format!("update_install: from cache v={}", latest_version));
                                    let ver = latest_version.clone();
                                    let result = tokio::task::spawn_blocking(move || {
                                        if let Some((_, cached_path)) =
                                            omnish_common::update::local_cached_package(&os_clone, &arch_clone)
                                        {
                                            omnish_common::update::extract_and_run_installer(&cached_path, &ver, true)
                                        } else {
                                            anyhow::bail!("cached package disappeared")
                                        }
                                    }).await;
                                    match result {
                                        Ok(Ok(())) => {
                                            event_log::push(format!("update_install: done v={}", latest_version));
                                            un.store(false, Ordering::Relaxed);
                                        }
                                        Ok(Err(e)) => {
                                            event_log::push(format!("update_install: failed {}", e));
                                            tracing::warn!("update install from cache failed: {}", e);
                                        }
                                        Err(e) => {
                                            event_log::push(format!("update_install: panic {}", e));
                                        }
                                    }
                                    uip.store(false, Ordering::Relaxed);
                                });
                            }
                        }
                        Ok(Message::UpdateInfo { available: false, .. }) => {
                            event_log::push("update_check: up to date");
                        }
                        Ok(other) => {
                            event_log::push(format!("update_check: unexpected {:?}", std::mem::discriminant(other)));
                        }
                        Err(e) => {
                            event_log::push(format!("update_check: error {}", e));
                        }
                    }
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
                        let exit_action = enter_chat_mode(
                            None, &daemon_conn, &mut chat_history, &mut last_thread_id,
                            &session_id, &shell, &proxy, &shell_input, &interceptor, &shell_completer,
                            &osc133_detector, &last_readline_content, &col_tracker,
                            &onboarded, locked, &config, Arc::clone(&sandbox_state),
                        ).await;
                        if let chat_session::ChatExitAction::Lock(lock) = exit_action {
                            handle_lock(&mut proxy, &mut master_fd, &mut locked, lock, &shell, &shell_args_ref, &session_id);
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
            deferred_ghost = None; // User typed - cancel pending ghost render

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
                        // Not a DSR byte - check if detector aborted mid-sequence
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

            // Flush bare ESC from DSR detector - a standalone ESC not followed
            // by '[' in the same read() is a user keypress, not a DSR response.
            if let Some(flushed) = dsr_detector.flush_bare_esc() {
                filtered_input.extend_from_slice(&flushed);
            }
            for &byte in &filtered_input {
                match interceptor.feed_byte(byte) {
                    InterceptAction::Buffering(buf) => {
                        if buf == prefix_bytes {
                            // Full prefix matched - start timer for double-prefix detection.
                            // No visual feedback yet; chat prompt appears on timeout or Enter.
                            shell_completer.clear();
                            prefix_match_time = Some(std::time::Instant::now());
                        } else if buf.len() > prefix_bytes.len() && buf.starts_with(&prefix_bytes) {
                            // Additional input after prefix - cancel timer
                            prefix_match_time = None;
                        }
                    }
                    InterceptAction::Backspace(_buf) => {
                        // No visual prompt to update - prefix buffering is invisible
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
                            // Bare ESC dismisses ghost text - consume the key (don't forward to PTY)
                            if shell_completer.dismiss() {
                                nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                ghost_wrapped = false;
                                if let Some(ref rpc) = daemon_conn {
                                    let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                                    send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
                                }
                            }
                        } else {
                            // Forward these bytes to PTY
                            proxy.write_all(&bytes)?;
                            // Track keystroke for auto-update idle detection
                            last_keystroke = std::time::Instant::now();

                            if shell_input.at_prompt() {
                                if needs_readline_report(&bytes) {
                                    // Tab, Up, Down modify readline state - send
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
                                        nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                        ghost_wrapped = false;
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
                                        nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                        ghost_wrapped = false;
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
                                    // Ghost was cleared - erase stale ghost text from screen
                                    nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                    ghost_wrapped = false;
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
                        // ESC pressed - reset state, no UI to dismiss
                        prefix_match_time = None;
                        completer.clear();
                    }
                    InterceptAction::Chat(msg) => {
                        prefix_match_time = None;
                        event_log::push("chat mode enter");
                        completer.clear();
                        let initial = if msg.trim().is_empty() { None } else { Some(msg) };
                        let exit_action = enter_chat_mode(
                            initial, &daemon_conn, &mut chat_history, &mut last_thread_id,
                            &session_id, &shell, &proxy, &shell_input, &interceptor, &shell_completer,
                            &osc133_detector, &last_readline_content, &col_tracker,
                            &onboarded, locked, &config, Arc::clone(&sandbox_state),
                        ).await;
                        if let chat_session::ChatExitAction::Lock(lock) = exit_action {
                            handle_lock(&mut proxy, &mut master_fd, &mut locked, lock, &shell, &shell_args_ref, &session_id);
                        }
                    }
                    InterceptAction::ResumeChat => {
                        let gap_ms = prefix_match_time.map(|t| t.elapsed().as_millis()).unwrap_or(0);
                        prefix_match_time = None;
                        event_log::push(format!("chat mode resume (double-prefix, gap {}ms)", gap_ms));
                        completer.clear();
                        let resume_cmd = match last_thread_id {
                            Some(ref tid) => format!("/resume_tid {}", tid),
                            None => "/resume".to_string(),
                        };
                        let exit_action = enter_chat_mode(
                            Some(resume_cmd), &daemon_conn, &mut chat_history, &mut last_thread_id,
                            &session_id, &shell, &proxy, &shell_input, &interceptor, &shell_completer,
                            &osc133_detector, &last_readline_content, &col_tracker,
                            &onboarded, locked, &config, Arc::clone(&sandbox_state),
                        ).await;
                        if let chat_session::ChatExitAction::Lock(lock) = exit_action {
                            handle_lock(&mut proxy, &mut master_fd, &mut locked, lock, &shell, &shell_args_ref, &session_id);
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
                        // ESC sequence in progress - no UI update needed
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
                        // Bare ESC dismisses ghost text - consume the key
                        if bytes == [0x1b] && shell_completer.ghost().is_some() {
                            if shell_completer.dismiss() {
                                nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                ghost_wrapped = false;
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

                    // Track cursor position on display (stripped) data - must happen
                    // before ghost rendering so col_tracker.col reflects where the
                    // ghost text starts (needed for wrap detection).
                    col_tracker.feed(display_data);
                    notice_queue::set_cursor_row(col_tracker.row);

                    // Render ghost text after display_data write.
                    // Priority 1: deferred ghost (just set during this or prior RL processing).
                    // Priority 2: re-render active ghost - handles the case where bash's
                    // readline redraw is split across multiple PTY reads and a trailing
                    // fragment overwrites the previously-rendered ghost.
                    if let Some(ghost_render) = deferred_ghost.take() {
                        let cols = get_terminal_size().map(|(_, c)| c as usize).unwrap_or(80);
                        ghost_wrapped = col_tracker.col as usize + deferred_ghost_width > cols;
                        nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                    } else if !display_data.is_empty()
                        && shell_input.at_prompt()
                        && shell_input.cursor_at_end()
                    {
                        if let Some(suffix) = shell_completer.ghost() {
                            let cols = get_terminal_size().map(|(_, c)| c as usize).unwrap_or(80);
                            ghost_wrapped = col_tracker.col as usize + display::display_width(suffix) > cols;
                            let ghost_render = display::render_ghost_text(suffix);
                            nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                        }
                    }

                    // Detect alternate screen transitions
                    if let Some(active) = alt_screen_detector.feed(display_data) {
                        interceptor.set_suppressed(active);
                        notice_queue::set_alt_screen(active);
                    }

                    // Notify interceptor of output (resets chat state)
                    interceptor.note_output(display_data);

                    // Send IoData to daemon (throttled) - skip while alternate screen
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
                                pending_completion_responses.clear();
                                readline_triggered_for_completions = false;
                                readline_trigger_time = None;
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
                                pending_completion_responses.clear();
                                readline_triggered_for_completions = false;
                                readline_trigger_time = None;
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
                                if !command_tracker.has_pending() {
                                    event_log::push("WARNING: CommandStart without pending (recovery will create one)");
                                }
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
                                pending_completion_responses.clear();
                                readline_triggered_for_completions = false;
                                readline_trigger_time = None;
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
                                pending_completion_responses.clear();
                                readline_triggered_for_completions = false;
                                readline_trigger_time = None;
                            }
                            Osc133EventKind::ReadlineLine { content, point } => {
                                event_log::push(format!(
                                    "readline response content={:?} point={:?}",
                                    content, point
                                ));
                                shell_input.set_readline(content, *point);
                                interceptor.update_readline(content);
                                last_readline_content = Some(content.to_string());

                                // Process any pending completion responses now that we have latest input
                                if !pending_completion_responses.is_empty() {
                                    if shell_input.cursor_at_end() {
                                        let current = shell_input.input();
                                        for resp in pending_completion_responses.drain(..) {
                                            if let Some(ghost) = shell_completer.on_response(&resp, current) {
                                                // Defer rendering until after bash's readline
                                                // redraw (which arrives in the next PTY read).
                                                deferred_ghost_width = display::display_width(ghost);
                                                deferred_ghost = Some(display::render_ghost_text(ghost));
                                            }
                                        }
                                    } else {
                                        // Cursor not at end - discard pending completions
                                        pending_completion_responses.clear();
                                        if shell_completer.ghost().is_some() {
                                            shell_completer.clear();
                                            nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                            ghost_wrapped = false;
                                        }
                                    }
                                    readline_triggered_for_completions = false;
                                    readline_trigger_time = None;
                                }

                                if let Some((input, seq)) = shell_input.take_change() {
                                    let had_ghost = shell_completer.ghost().is_some();
                                    if shell_completer.on_input_changed(input, seq) {
                                        event_log::push(format!("on_input_changed cleared ghost input={:?}", input));
                                        nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
                                        ghost_wrapped = false;
                                    } else if had_ghost {
                                        event_log::push(format!("on_input_changed kept ghost input={:?}", input));
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
                        event_log::push("OSC 133 shell hook not active, falling back to regex prompt detection");
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

                    // Flush deferred ghost if set during this PTY read's OSC processing,
                    // but ONLY if display_data was non-empty (meaning the readline
                    // Flush deferred ghost after OSC processing. Previously guarded
                    // by !display_data.is_empty() (waiting for bash readline redraw),
                    // but zsh's ZLE doesn't send a redraw after widget execution.
                    // Ghost text uses DECSC/DECRC so cursor position is preserved.
                    if let Some(ghost_render) = deferred_ghost.take() {
                        let cols = get_terminal_size().map(|(_, c)| c as usize).unwrap_or(80);
                        ghost_wrapped = col_tracker.col as usize + deferred_ghost_width > cols;
                        event_log::push("flushing deferred ghost (post-OSC)");
                        nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
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

            if completion_enabled && at_prompt && !in_chat && !shell_input.in_isearch() && shell_input.cursor_at_end() && shell_completer.should_request(shell_input.sequence_id(), current) {
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
        // Discard responses if user has entered chat mode, isearch mode,
        // or is no longer at prompt (command executing). Issue #507.
        while let Ok(resp) = completion_rx.try_recv() {
            if interceptor.is_in_chat() {
                shell_completer.clear();
                continue;
            }

            // Discard responses that arrive after user has left the prompt
            // (e.g. command is executing). The response is stale. (issue #507)
            if !shell_input.at_prompt() {
                event_log::push(format!("completion response seq={} discarded (not at prompt)", resp.sequence_id));
                continue;
            }

            // In isearch mode (Ctrl+R) - discard to avoid "cannot find keymap" error (issue #88)
            if shell_input.in_isearch() || shell_input.pending_rl_report() {
                event_log::push(format!("completion response seq={} discarded (isearch)", resp.sequence_id));
                continue;
            }

            event_log::push(format!("completion response seq={} suggestions={:?}",
                resp.sequence_id,
                resp.suggestions.iter().map(|s| &s.text).collect::<Vec<_>>()));
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
                    // Guard: only render ghost text when at prompt (issue #507)
                    if shell_input.at_prompt() && shell_input.cursor_at_end() {
                        let current = shell_input.input();
                        for resp in pending_completion_responses.drain(..) {
                            if let Some(ghost) = shell_completer.on_response(&resp, current) {
                                let cols = get_terminal_size().map(|(_, c)| c as usize).unwrap_or(80);
                                ghost_wrapped = col_tracker.col as usize + display::display_width(ghost) > cols;
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

        // Check for pushed config changes from daemon (non-blocking)
        if let Some(ref rpc) = daemon_conn {
            while let Some(msg) = rpc.try_recv_push().await {
                if let Message::ConfigClient { changes } = msg {
                    apply_client_config_changes(
                        &changes,
                        &mut interceptor,
                        &mut completion_enabled,
                        &mut ghost_timeout_ms,
                        &mut prefix_bytes,
                    );
                }
            }
        }

        // Auto-dismiss expired ghost text
        if shell_completer.is_ghost_expired(ghost_timeout_ms) {
            // Send completion summary (ignored - ghost expired)
            if let Some(ref rpc) = daemon_conn {
                let shell_cwd = get_shell_cwd(proxy.child_pid() as u32);
                send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd);
            }
            shell_completer.clear();
            nix::unistd::write(std::io::stdout(), display::erase_ghost_text(ghost_wrapped)).ok();
            ghost_wrapped = false;
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

fn apply_client_config_changes(
    changes: &[omnish_protocol::message::ConfigChange],
    interceptor: &mut InputInterceptor,
    completion_enabled: &mut bool,
    ghost_timeout_ms: &mut u64,
    prefix_bytes: &mut Vec<u8>,
) {
    let mut any_changed = false;
    for change in changes {
        match change.path.as_str() {
            "client.command_prefix" => {
                interceptor.update_prefix(&change.value);
                *prefix_bytes = change.value.as_bytes().to_vec();
                any_changed = true;
            }
            "client.resume_prefix" => {
                interceptor.update_resume_prefix(&change.value);
                any_changed = true;
            }
            "client.completion_enabled" => {
                if let Ok(v) = change.value.parse::<bool>() {
                    *completion_enabled = v;
                    any_changed = true;
                }
            }
            "client.ghost_timeout_ms" => {
                if let Ok(v) = change.value.parse::<u64>() {
                    *ghost_timeout_ms = v;
                    any_changed = true;
                }
            }
            "client.intercept_gap_ms" => {
                if let Ok(v) = change.value.parse::<u64>() {
                    interceptor.update_min_gap(std::time::Duration::from_millis(v));
                    any_changed = true;
                }
            }
            "client.developer_mode" => {
                if let Ok(v) = change.value.parse::<bool>() {
                    interceptor.set_developer_mode(v);
                    any_changed = true;
                }
            }
            "client.language" => {
                // OMNISH_LANG env var overrides daemon-pushed language
                if std::env::var("OMNISH_LANG").is_err() {
                    i18n::init(&change.value);
                }
                any_changed = true;
            }
            _ => {} // unknown paths silently ignored
        }
    }
    if any_changed {
        let changes = changes.to_vec();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = save_client_config_cache(&changes) {
                tracing::warn!("failed to cache client config: {}", e);
            }
        });
    }
}

fn save_client_config_cache(changes: &[omnish_protocol::message::ConfigChange]) -> anyhow::Result<()> {
    use fs2::FileExt;

    let path = std::env::var("OMNISH_CLIENT_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"));

    // Exclusive lock so multiple clients don't clobber each other.
    let lock_path = path.with_extension("toml.lock");
    let lock_file = std::fs::File::create(&lock_path)?;
    lock_file.lock_exclusive()?;

    // Read current file content once, then apply all changes in memory.
    let content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut doc = content.parse::<toml_edit::DocumentMut>()?;

    let mut any_modified = false;
    for change in changes {
        // Map daemon.toml "client.*" paths to client.toml keys.
        // Use typed values to preserve TOML type correctness.
        let (key, new_item) = match change.path.as_str() {
            "client.command_prefix" => ("shell.command_prefix", toml_edit::value(&change.value)),
            "client.resume_prefix" => ("shell.resume_prefix", toml_edit::value(&change.value)),
            "client.completion_enabled" => {
                let v: bool = change.value.parse().unwrap_or(false);
                ("shell.completion_enabled", toml_edit::value(v))
            }
            "client.developer_mode" => {
                let v: bool = change.value.parse().unwrap_or(false);
                ("shell.developer_mode", toml_edit::value(v))
            }
            "client.ghost_timeout_ms" => {
                if let Ok(v) = change.value.parse::<i64>() {
                    ("shell.ghost_timeout_ms", toml_edit::value(v))
                } else {
                    continue;
                }
            }
            "client.intercept_gap_ms" => {
                if let Ok(v) = change.value.parse::<i64>() {
                    ("shell.intercept_gap_ms", toml_edit::value(v))
                } else {
                    continue;
                }
            }
            "client.language" => ("shell.language", toml_edit::value(&change.value)),
            _ => continue,
        };

        // Navigate to the leaf and compare before overwriting.
        let segments = omnish_common::config_edit::split_key_path(key);
        let (parents, leaf) = segments.split_at(segments.len() - 1);
        let mut table = doc.as_table_mut();
        for seg in parents {
            if !table.contains_key(seg) {
                table.insert(seg, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table[seg.as_str()]
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("{} is not a table", seg))?;
        }
        let leaf_key = &leaf[0];
        // Skip if the value already matches (compare by typed extraction).
        if let Some(existing) = table.get(leaf_key) {
            if let (Some(a), Some(b)) = (existing.as_value(), new_item.as_value()) {
                let same = match (a.as_str(), b.as_str()) {
                    (Some(a), Some(b)) => a == b,
                    _ => match (a.as_bool(), b.as_bool()) {
                        (Some(a), Some(b)) => a == b,
                        _ => match (a.as_integer(), b.as_integer()) {
                            (Some(a), Some(b)) => a == b,
                            _ => false,
                        },
                    },
                };
                if same {
                    continue;
                }
            }
        }
        table[leaf_key] = new_item;
        any_modified = true;
    }

    if !any_modified {
        return Ok(());
    }

    // Atomic write: write to temp file then rename.
    let output = doc.to_string();
    let output = if output.ends_with('\n') { output } else { format!("{}\n", output) };
    let tmp_path = path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &output)?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

async fn connect_daemon(
    daemon_addr: &str,
    session_id: &str,
    parent_session_id: Option<String>,
    child_pid: u32,
    buffer: MessageBuffer,
    update_needed: Arc<AtomicBool>,
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

    match RpcClient::connect_with_reconnect_full(
        &socket_path,
        tls_connector,
        move |rpc| {
            let sid = sid.clone();
            let psid = psid.clone();
            let rpc = rpc.clone();
            let buffer = buffer.clone();
            let token = auth_token.clone();
            let update_needed = update_needed.clone();
            Box::pin(async move {
                event_log::push("reconnect_cb: authenticating");
                // Authenticate first
                let auth_resp = rpc.call(Message::Auth(Auth {
                    token,
                    protocol_version: omnish_protocol::message::PROTOCOL_VERSION,
                })).await;
                let auth_resp = match auth_resp {
                    Ok(resp) => resp,
                    Err(e) => {
                        event_log::push(format!("reconnect_cb: auth call failed: {}", e));
                        anyhow::bail!("auth call failed: {}", e);
                    }
                };
                match &auth_resp {
                    Message::AuthResult(result) => {
                        event_log::push(format!(
                            "reconnect_cb: auth ok={} proto={} daemon={}",
                            result.ok, result.protocol_version, result.daemon_version
                        ));

                        // Check if daemon has a newer version → trigger update
                        if !result.daemon_version.is_empty()
                            && compare_versions(&result.daemon_version, omnish_common::VERSION)
                                == std::cmp::Ordering::Greater
                        {
                            event_log::push(format!(
                                "reconnect_cb: daemon newer ({}), triggering update",
                                result.daemon_version
                            ));
                            update_needed.store(true, Ordering::Relaxed);
                        }

                        if !result.ok {
                            let behind = if omnish_protocol::message::PROTOCOL_VERSION < result.protocol_version {
                                "client"
                            } else {
                                "daemon"
                            };
                            notice(&format!(
                                "[omnish] Protocol mismatch \
                                 (client={}, daemon={}), waiting for {} upgrade...",
                                omnish_protocol::message::PROTOCOL_VERSION,
                                result.protocol_version,
                                behind
                            ));
                            // Don't fail - keep connection alive for update messages
                            return Ok(());
                        }
                    }
                    // Old daemon that responds with Ack or unexpected message
                    other => {
                        event_log::push(format!("reconnect_cb: auth unexpected {:?}", std::mem::discriminant(other)));
                    }
                }

                // Then register session (only if auth succeeded)
                let attrs = probe::default_session_probes(child_pid).collect_all();
                event_log::push("reconnect_cb: sending SessionStart");
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
                if !buffered.is_empty() {
                    event_log::push(format!("reconnect_cb: replaying {} buffered msgs", buffered.len()));
                }
                for msg in buffered {
                    if rpc.call(msg).await.is_err() {
                        event_log::push("reconnect_cb: replay failed");
                        break; // Connection broke again during replay
                    }
                }
                event_log::push("reconnect_cb: done");
                Ok(())
            })
        },
        Some(|| {
            event_log::push("reconnect: connection restored");
            notice("[omnish] reconnected to daemon");
        }),
        Some(|| {
            event_log::push("disconnect: connection lost to daemon");
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

/// Respawn the shell with or without sandbox restrictions.
/// Uses the unified sandbox backend (bwrap/landlock/seatbelt).
fn handle_lock(
    proxy: &mut PtyProxy,
    master_fd: &mut i32,
    locked: &mut bool,
    lock: bool,
    shell: &str,
    shell_args: &[&str],
    session_id: &str,
) {
    if lock == *locked {
        let status = if lock { "already locked" } else { "already unlocked" };
        let msg = format!("\r\n{}{}{}\r\n", display::YELLOW, status, display::RESET);
        nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
        return;
    }

    // Get current cwd from shell process before killing it
    let cwd = get_shell_cwd(proxy.child_pid() as u32)
        .map(std::path::PathBuf::from);

    let mut env = std::collections::HashMap::new();
    env.insert("OMNISH_SESSION_ID".to_string(), session_id.to_string());
    env.insert("SHELL".to_string(), shell.to_string());

    if lock {
        // Determine sandbox backend
        let preferred = if cfg!(target_os = "macos") {
            omnish_plugin::SandboxBackendType::from_config("macos")
        } else {
            omnish_plugin::SandboxBackendType::from_config("bwrap")
        };
        let backend = preferred.and_then(omnish_plugin::detect_backend);
        let policy = omnish_plugin::lock_policy(cwd.as_deref());

        match backend {
            Some(omnish_plugin::SandboxBackendType::Landlock) => {
                // Landlock applies via pre_exec in the forked child
                let cwd_clone = cwd.clone();
                let pre_exec: Option<Box<dyn FnOnce() -> Result<(), String> + Send>> =
                    Some(Box::new(move || {
                        let policy = omnish_plugin::lock_policy(cwd_clone.as_deref());
                        omnish_plugin::apply_in_process(&policy)
                    }));
                do_respawn(proxy, master_fd, locked, shell, shell_args, env, cwd.as_deref(), pre_exec, true);
            }
            Some(backend) => {
                // Bwrap/seatbelt: build a sandbox command wrapping the shell
                let shell_path = std::path::Path::new(shell);
                let args_refs: Vec<&str> = shell_args.to_vec();
                match omnish_plugin::sandbox_command(backend, &policy, shell_path, &args_refs) {
                    Ok(cmd) => {
                        let program = cmd.get_program().to_string_lossy().to_string();
                        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
                        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        do_respawn(proxy, master_fd, locked, &program, &args_refs, env, cwd.as_deref(), None, true);
                    }
                    Err(e) => {
                        let msg = format!("\r\n{}Sandbox setup failed: {}{}\r\n", display::RED, e, display::RESET);
                        nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
                    }
                }
            }
            None => {
                let msg = format!(
                    "\r\n{}No sandbox backend available, cannot lock shell{}\r\n",
                    display::YELLOW, display::RESET
                );
                nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
            }
        }
    } else {
        // Unlock: respawn shell without sandbox
        do_respawn(proxy, master_fd, locked, shell, shell_args, env, cwd.as_deref(), None, false);
    }
}

/// Helper to respawn the shell and update lock state.
#[allow(clippy::too_many_arguments)]
fn do_respawn(
    proxy: &mut PtyProxy,
    master_fd: &mut i32,
    locked: &mut bool,
    cmd: &str,
    args: &[&str],
    env: std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    pre_exec: Option<Box<dyn FnOnce() -> Result<(), String> + Send>>,
    lock: bool,
) {
    match proxy.respawn(cmd, args, env, cwd, pre_exec) {
        Ok(new_fd) => {
            *master_fd = new_fd;
            *locked = lock;
            setup_sigwinch(new_fd);
            if let Some((rows, cols)) = get_terminal_size() {
                proxy.set_window_size(rows, cols).ok();
            }
            let status = if lock { "locked" } else { "unlocked" };
            event_log::push(format!("lock: shell respawned ({})", status));
            let msg = format!("\r\n{}Shell {}{}\r\n", display::GREEN, status, display::RESET);
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
        }
        Err(e) => {
            let msg = format!("\r\n{}Failed to respawn shell: {}{}\r\n", display::RED, e, display::RESET);
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
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
                        // Complete character - decode and measure width
                        if let Ok(s) = std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize]) {
                            if let Some(ch) = s.chars().next() {
                                self.col += ch.width().unwrap_or(0) as u16;
                            }
                        }
                        self.utf8_need = 0;
                        self.utf8_len = 0;
                    }
                } else {
                    // Invalid continuation - discard partial and re-process this byte
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
            // CUU - Cursor Up: \x1b[nA
            b'A' => {
                let n = self.parse_csi_param_1().max(1);
                self.row = self.row.saturating_sub(n);
            }
            // CUB - Cursor Back: \x1b[nD  (handled here for completeness)
            // CUD - Cursor Down: \x1b[nB
            b'B' => {
                let n = self.parse_csi_param_1().max(1);
                self.row = self.row.saturating_add(n);
            }
            // CUP / HVP - Cursor Position: \x1b[n;mH or \x1b[n;mf
            b'H' | b'f' => {
                let (r, c) = self.parse_csi_param_2();
                // CSI params are 1-based, convert to 0-based
                self.row = r.max(1) - 1;
                self.col = c.max(1) - 1;
            }
            // SD - Scroll Down: \x1b[nT - content moves down, cursor row unchanged
            // but conceptually row 0 content is now new
            // SU - Scroll Up: \x1b[nS - content moves up
            // IL - Insert Line: \x1b[nL - inserts lines at cursor, pushes down
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
            // UTF-8 start bytes - begin accumulation
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
    /// - `Some(Some((row, col)))` - complete DSR response parsed, byte consumed
    /// - `Some(None)` - byte is part of an in-progress DSR response, consumed
    /// - `None` - byte is not part of a DSR response, should be forwarded
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
                    // Not a CSI - abort, bytes need to be replayed
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

/// Build a one-time sandbox notice for the current chat entry.
/// Returns `None` when the preferred backend is available (no notice needed).
fn sandbox_notice(status: omnish_plugin::SandboxDetectResult) -> Option<String> {
    use omnish_plugin::SandboxDetectResult;
    match status {
        SandboxDetectResult::Preferred(_) => None,
        SandboxDetectResult::Fallback { preferred, actual } => {
            let hint = bwrap_hint(preferred);
            Some(format!(
                "\r\n{}[omnish] sandbox: {:?} not available, falling back to {:?}.{}{}\r\n",
                display::DIM, preferred, actual, hint, display::RESET,
            ))
        }
        SandboxDetectResult::Unavailable { preferred } => {
            let hint = bwrap_hint(preferred);
            Some(format!(
                "\r\n{}[omnish] sandbox: no backend available, tool execution is not sandboxed.{}{}\r\n",
                display::DIM, hint, display::RESET,
            ))
        }
        SandboxDetectResult::Disabled => {
            Some(format!(
                "\r\n{}[omnish] sandbox: disabled by client config, tool execution is not sandboxed.{}\r\n",
                display::DIM, display::RESET,
            ))
        }
    }
}

fn bwrap_hint(preferred: omnish_plugin::SandboxBackendType) -> &'static str {
    use omnish_plugin::SandboxBackendType;
    #[cfg(not(target_os = "macos"))]
    use omnish_plugin::BwrapUnavailableReason;
    if preferred != SandboxBackendType::Bwrap {
        return "";
    }
    #[cfg(not(target_os = "macos"))]
    match omnish_plugin::bwrap_unavailable_reason() {
        Some(BwrapUnavailableReason::NotInstalled) => {
            " Install bwrap: sudo apt install bubblewrap"
        }
        Some(BwrapUnavailableReason::NamespaceDenied) => {
            " bwrap blocked by AppArmor. To allow: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0"
        }
        None => "",
    }
    #[cfg(target_os = "macos")]
    ""
}

/// Unified entry point for chat mode (new chat, resume, or timeout).
#[allow(clippy::too_many_arguments)]
async fn enter_chat_mode(
    initial_msg: Option<String>,
    daemon_conn: &Option<RpcClient>,
    chat_history: &mut VecDeque<String>,
    last_thread_id: &mut Option<String>,
    session_id: &str,
    shell: &str,
    proxy: &PtyProxy,
    shell_input: &shell_input::ShellInputTracker,
    interceptor: &interceptor::InputInterceptor,
    shell_completer: &completion::ShellCompleter,
    osc133_detector: &omnish_tracker::osc133_detector::Osc133Detector,
    last_readline_content: &Option<String>,
    col_tracker: &CursorTracker,
    onboarded: &AtomicBool,
    locked: bool,
    config: &omnish_common::config::ClientConfig,
    sandbox_state: Arc<RwLock<ClientSandboxConfig>>,
) -> chat_session::ChatExitAction {
    notice_queue::defer();
    let saved_input = shell_input.input().to_string();

    let (exit_action, pending_cd) = if let Some(ref rpc) = daemon_conn {
        let shell_pid = proxy.child_pid() as u32;
        let dbg_fn = || debug_client_state(
            shell_input, interceptor, shell_completer,
            daemon_conn, osc133_detector, last_readline_content,
            shell_pid, col_tracker, locked,
        );
        let action;
        let pending_cd;
        {
            let mut session = chat_session::ChatSession::new(
                std::mem::take(chat_history),
                config.shell.extended_unicode,
                Arc::clone(&sandbox_state),
            );

            // One-time sandbox notice per chat entry (#514)
            if let Some(msg) = sandbox_notice(session.sandbox_status()) {
                nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
            }

            action = session.run(rpc, session_id, proxy, initial_msg, &dbg_fn, onboarded, col_tracker.col, col_tracker.row).await;
            let new_tid = session.thread_id().map(String::from);
            event_log::push(format!("chat exit: last_thread_id {:?} -> {:?}", last_thread_id, new_tid));
            *last_thread_id = new_tid;
            // Keep env var in sync for exec_update
            if let Some(ref tid) = last_thread_id {
                std::env::set_var("OMNISH_LAST_THREAD_ID", tid);
            } else {
                std::env::remove_var("OMNISH_LAST_THREAD_ID");
            }
            pending_cd = session.pending_cd().map(String::from);
            *chat_history = session.into_history();
        }
        (action, pending_cd)
    } else {
        let err = display::render_error(i18n::t("error.daemon_not_connected"));
        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        (chat_session::ChatExitAction::Normal, None)
    };

    nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
    notice_queue::flush();
    // Clear the shell command line before restoring state.
    // Bash readline: Ctrl-U (kill backward) + Ctrl-K (kill forward)
    // Zsh ZLE: Ctrl-U alone does kill-whole-line (both directions);
    //   sending Ctrl-K to zsh may leak as "^K" command when ZLE isn't ready.
    if shell.ends_with("zsh") {
        proxy.write_all(b"\x15").ok();
    } else {
        proxy.write_all(b"\x15\x0b").ok();
    }
    // Execute pending cd (from resume mismatch) before restoring input
    if let Some(ref dir) = pending_cd {
        proxy.write_all(format!("cd {}\r", dir).as_bytes()).ok();
    } else {
        proxy.write_all(b"\r").ok();
    }
    if !saved_input.is_empty() {
        proxy.write_all(saved_input.as_bytes()).ok();
    }
    exit_action
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
    locked: bool,
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
    output.push_str(&format!("  locked: {}\n", locked));
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
                    Some(b'D') | Some(b'F') | Some(b'H') => return true, // Left / End / Home xterm (#518)
                    Some(b'1') if bytes.get(i + 3) == Some(&b'~') => return true, // Home VT (#518)
                    Some(b'4') if bytes.get(i + 3) == Some(&b'~') => return true, // End VT (#518)
                    _ => {}
                }
            }
            0x1b if bytes.get(i + 1) == Some(&b'O') => {
                match bytes.get(i + 2) {
                    Some(b'H') | Some(b'F') => return true, // Home / End SS3 (#518)
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
/// `cwd` is used to resolve relative redirect paths.
pub(crate) fn handle_command_result(content: &str, redirect: Option<&str>, cwd: Option<&str>) {
    if let Some(path) = redirect {
        let resolved_path = if std::path::Path::new(path).is_relative() {
            match cwd {
                Some(cwd) => std::path::Path::new(cwd).join(path),
                None => std::path::Path::new(path).to_path_buf(),
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
    cwd: Option<&str>,
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
            handle_command_result(&display, redirect, cwd);
            if show_thinking {
                let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                let separator = display::render_separator(cols);
                let sep_line = format!("{}\r\n", separator);
                nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
            }
        }
        _ => {
            nix::unistd::write(std::io::stdout(), status.clear().as_bytes()).ok();
            let err = display::render_error(i18n::t("error.failed_receive_response_main"));
            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
        }
    }
}

/// Handle a /command in chat mode. Returns true if the command was handled.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_slash_command(
    trimmed: &str,
    session_id: &str,
    rpc: &RpcClient,
    proxy: &PtyProxy,
    cwd: Option<&str>,
    client_debug_fn: &dyn Fn() -> String,
    cursor_col: u16,
    cursor_row: u16,
) -> bool {
    // /update is intercepted in DaemonQuery handling below
    // (it needs process state: proxy fd/pid)

    match command::dispatch(trimmed) {
        command::ChatAction::Command { result, redirect, limit } => {
            let display_result = if let Some(ref l) = limit {
                command::apply_limit(&result, l)
            } else {
                result
            };
            if let Some(path) = redirect.as_deref() {
                handle_command_result(&display_result, Some(path), cwd);
            } else {
                // Command output is plain text - skip markdown rendering
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
                    handle_command_result(&display_result, Some(path), cwd);
                } else {
                    // Plain text output - skip markdown rendering to preserve blank lines
                    let output = format!("\r\n{}\r\n", display_result.replace('\n', "\r\n"));
                    nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                }
                return true;
            } else if query == "__cmd:update" {
                let tid = std::env::var("OMNISH_LAST_THREAD_ID").ok().filter(|s| !s.is_empty());
                exec_update(proxy, session_id, cursor_col, cursor_row, tid.as_deref());
                return true; // Only reached if exec failed
            }
            if let Some(path) = redirect.as_deref() {
                send_daemon_query(&query, session_id, rpc, Some(path), false, cwd).await;
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
                        // Command output is plain text - skip markdown rendering
                        let output = format!("\r\n{}\r\n", display.replace('\n', "\r\n"));
                        nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                    }
                    _ => {
                        let err = display::render_error(i18n::t("error.failed_receive_response_main"));
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

        let mut interceptor = InputInterceptor::new(":", "::", Box::new(AlwaysIntercept), false);
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
        // Chinese characters are fullwidth - each occupies 2 columns
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
        // ❯ (U+276F) is narrow - width 1
        t.feed("❯ ".as_bytes());
        assert_eq!(t.col, 2); // ❯ (1) + space (1)

        // 🚀 (U+1F680) is a wide emoji - width 2
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
        // \x1b[H - cursor to (0,0)
        t.feed(b"\x1b[H");
        assert_eq!(t.row, 0);
        assert_eq!(t.col, 0);
    }

    #[test]
    fn test_row_tracker_cup_with_params() {
        let mut t = CursorTracker::new();
        // \x1b[5;10H - cursor to row 5, col 10 (1-based → 4, 9 zero-based)
        t.feed(b"\x1b[5;10H");
        assert_eq!(t.row, 4);
        assert_eq!(t.col, 9);
    }

    #[test]
    fn test_row_tracker_cursor_up_down() {
        let mut t = CursorTracker::new();
        t.feed(b"\n\n\n\n\n"); // row = 5
        assert_eq!(t.row, 5);
        // \x1b[2A - cursor up 2
        t.feed(b"\x1b[2A");
        assert_eq!(t.row, 3);
        // \x1b[B - cursor down 1 (no param = 1)
        t.feed(b"\x1b[B");
        assert_eq!(t.row, 4);
    }

    #[test]
    fn test_row_tracker_cursor_up_saturates() {
        let mut t = CursorTracker::new();
        t.feed(b"\n"); // row = 1
        // Move up 10 - should saturate to 0
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
        assert_eq!(d.feed(b'O'), None);        // not '[', abort - replay
        assert!(!d.buf.is_empty());
        let replay = d.take_buf();
        assert_eq!(replay, vec![0x1b, b'O']);
    }

    #[test]
    fn test_dsr_non_r_final_aborts() {
        let mut d = DsrDetector::new();
        // \x1b[2A - cursor up, not a DSR response
        assert_eq!(d.feed(0x1b), Some(None));
        assert_eq!(d.feed(b'['), Some(None));
        assert_eq!(d.feed(b'2'), Some(None));
        assert_eq!(d.feed(b'A'), None); // final byte but not 'R' - abort
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
