// crates/omnish-client/src/main.rs
mod command;
mod completion;
pub mod event_log;
mod ghost_complete;
mod display;
mod interceptor;
mod probe;
mod shell_hook;
mod shell_input;
mod throttle;
mod util;

use anyhow::Result;
use omnish_common::config::load_client_config;
use interceptor::{InputInterceptor, InterceptAction, TimeGapGuard};
use omnish_protocol::message::*;
use omnish_pty::proxy::PtyProxy;
use omnish_pty::raw_mode::RawModeGuard;
use omnish_transport::rpc_client::RpcClient;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

type MessageBuffer = Arc<Mutex<VecDeque<Message>>>;

const MAX_BUFFER_SIZE: usize = 10_000;

fn should_buffer(msg: &Message) -> bool {
    matches!(msg, Message::IoData(_) | Message::CommandComplete(_) | Message::SessionUpdate(_))
}

/// Send completion summary to daemon if there's a pending completion
async fn send_completion_summary(
    rpc: &RpcClient,
    shell_completer: &mut completion::ShellCompleter,
    session_id: &str,
    accepted: bool,
    cwd: Option<String>,
) {
    if let Some(summary) = shell_completer.take_completion_summary(session_id, accepted, cwd) {
        let msg = Message::CompletionSummary(summary);
        let _ = rpc.call(msg).await;
    }
}

/// Send completion summary for ignored completion (accepted=false)
async fn send_ignored_summary(
    rpc: &RpcClient,
    shell_completer: &mut completion::ShellCompleter,
    session_id: &str,
    cwd: Option<String>,
) {
    // take_completion_summary returns None if there's no pending completion
    send_completion_summary(rpc, shell_completer, session_id, false, cwd).await;
}

/// Send a message to the daemon, buffering it if the send fails and
/// the message type is eligible for retry.
async fn send_or_buffer(rpc: &RpcClient, msg: Message, buffer: &MessageBuffer) {
    if rpc.call(msg.clone()).await.is_err() && should_buffer(&msg) {
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
fn get_shell_cwd(pid: u32) -> Option<String> {
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

#[tokio::main(worker_threads = 4)]
async fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("omnish {}", omnish_common::VERSION);
        return Ok(());
    }

    let config = load_client_config().unwrap_or_default();

    let session_id = Uuid::new_v4().to_string()[..8].to_string();
    let parent_session_id = std::env::var("OMNISH_SESSION_ID").ok();
    let shell = resolve_shell(&config.shell.command);
    let daemon_addr = std::env::var("OMNISH_SOCKET")
        .unwrap_or_else(|_| config.daemon_addr.clone());

    // Spawn PTY with shell, passing our session_id so nested omnish can detect parent.
    // Override $SHELL in child so programs that read it (e.g. tmux) don't re-launch omnish.
    let mut child_env = HashMap::new();
    child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
    child_env.insert("SHELL".to_string(), shell.clone());

    // Install shell hooks for OSC 133 support
    let osc133_rcfile = shell_hook::install_bash_hook(&shell);
    let osc133_hook_installed = osc133_rcfile.is_some();

    let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
        vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
    } else {
        vec![]
    };
    let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();
    let proxy = PtyProxy::spawn_with_env(&shell, &shell_args_ref, child_env)?;

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
    let mut interceptor = InputInterceptor::new(&config.shell.command_prefix, Box::new(guard));
    let prefix_bytes = config.shell.command_prefix.as_bytes();
    let mut alt_screen_detector = AltScreenDetector::new();
    let mut col_tracker = CursorColTracker::new();
    let mut dismiss_col: u16 = 0;
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
    let mut command_tracker = omnish_tracker::command_tracker::CommandTracker::new(
        session_id.clone(), cwd,
    );
    let mut throttle = throttle::OutputThrottle::new();
    let mut osc133_detector = omnish_tracker::osc133_detector::Osc133Detector::new();
    let mut osc133_warned = false;
    let mut completer = ghost_complete::GhostCompleter::new(vec![
        Box::new(ghost_complete::BuiltinProvider::new()),
    ]);
    let mut shell_input = shell_input::ShellInputTracker::new();
    let mut last_readline_content: Option<String> = None;
    // Pending completion responses waiting for readline report
    let mut pending_completion_responses: Vec<omnish_protocol::message::CompletionResponse> = Vec::new();
    // Whether we've triggered a readline report for pending completions
    let mut readline_triggered_for_completions = false;
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

        // Stdin -> PTY master
        if fds[0].revents & libc::POLLIN != 0 {
            let n = nix::unistd::read(0, &mut input_buf)?;
            if n == 0 {
                break;
            }

            // Suppress interceptor when not at prompt (child process running:
            // ssh, python REPL, etc.) so ':' is forwarded to the child.
            // Alt screen detector is handled separately in the output path.
            if !alt_screen_detector.is_active() {
                interceptor.set_suppressed(!shell_input.at_prompt());
            }

            // Feed bytes to interceptor one by one
            for i in 0..n {
                let byte = input_buf[i];
                match interceptor.feed_byte(byte) {
                    InterceptAction::Buffering(buf) => {
                        if buf == prefix_bytes {
                            // Save cursor column before drawing omnish UI
                            dismiss_col = col_tracker.col;
                            shell_completer.clear();
                            let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                            let prompt = display::render_prompt(cols);
                            nix::unistd::write(std::io::stdout(), prompt.as_bytes()).ok();
                        } else if buf.len() > prefix_bytes.len() && buf.starts_with(prefix_bytes) {
                            // Echo the user's input after the prompt
                            let user_input = &buf[prefix_bytes.len()..];
                            let echo = display::render_input_echo(user_input);
                            nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();

                            // Query completer for ghost text
                            if let Ok(input_str) = std::str::from_utf8(user_input) {
                                if let Some(ghost) = completer.update(input_str) {
                                    let ghost_render = display::render_ghost_text(ghost);
                                    nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                                }
                            }
                        }
                    }
                    InterceptAction::Backspace(buf) => {
                        if buf.is_empty() {
                            // Backspaced past the prefix — clear omnish UI, restore cursor column
                            let dismiss = display::render_dismiss();
                            let restore = format!("\x1b[{}G", dismiss_col + 1);
                            nix::unistd::write(std::io::stdout(), dismiss.as_bytes()).ok();
                            nix::unistd::write(std::io::stdout(), restore.as_bytes()).ok();
                        } else if buf.starts_with(prefix_bytes) {
                            if buf.len() == prefix_bytes.len() {
                                // Only prefix left — redraw ❯ with no input text
                                let echo = display::render_input_echo(b"");
                                nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();
                            } else {
                                // Show the user's input after the prompt
                                let user_input = &buf[prefix_bytes.len()..];
                                let echo = display::render_input_echo(user_input);
                                nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();
                            }
                        }
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
                                    send_completion_summary(rpc, &mut shell_completer, &session_id, true, shell_cwd).await;
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
                                            send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd).await;
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
                                            send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd).await;
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
                                        send_ignored_summary(rpc, &mut shell_completer, &session_id, shell_cwd).await;
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
                        // ESC pressed — clear omnish UI, restore cursor column
                        completer.clear();
                        let dismiss = display::render_dismiss();
                        let restore = format!("\x1b[{}G", dismiss_col + 1); // CHA is 1-indexed
                        nix::unistd::write(std::io::stdout(), dismiss.as_bytes()).ok();
                        nix::unistd::write(std::io::stdout(), restore.as_bytes()).ok();
                    }
                    InterceptAction::Chat(msg) => {
                        event_log::push("chat mode enter");
                        completer.clear();
                        // Save pre-chat input to restore after chat (issue #24)
                        let saved_input = shell_input.input().to_string();

                        // Enter chat mode loop (pass initial message if any)
                        if let Some(ref rpc) = daemon_conn {
                            let initial = if msg.trim().is_empty() { None } else { Some(msg) };
                            let dbg_fn = || debug_client_state(
                                &shell_input,
                                &interceptor,
                                &shell_completer,
                                &daemon_conn,
                                &osc133_detector,
                                &last_readline_content,
                            );
                            run_chat_loop(rpc, &session_id, &proxy, initial, &dbg_fn).await;
                        } else {
                            let err = display::render_error("Daemon not connected");
                            nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        }

                        // Clear bash readline before restoring.
                        // Ctrl-U (kill backward) + Ctrl-K (kill forward) + Enter
                        // to clear regardless of cursor position (issue #125).
                        proxy.write_all(b"\x15\x0b\r").ok();
                        // Restore pre-chat input so user doesn't lose their work (issue #24)
                        if !saved_input.is_empty() {
                            proxy.write_all(saved_input.as_bytes()).ok();
                        }
                    }
                    InterceptAction::Tab(_buf) => {
                        // Check if completer has a suggestion to accept
                        if let Some(suffix) = completer.accept() {
                            // Append suffix bytes to interceptor buffer
                            for &b in suffix.as_bytes() {
                                interceptor.inject_byte(b);
                            }
                            // Re-render with updated buffer
                            let new_buf = interceptor.current_buffer();
                            if new_buf.len() > prefix_bytes.len() && new_buf.starts_with(prefix_bytes) {
                                let user_input = &new_buf[prefix_bytes.len()..];
                                let echo = display::render_input_echo(user_input);
                                nix::unistd::write(std::io::stdout(), echo.as_bytes()).ok();

                                // Query for next ghost after accepting
                                if let Ok(input_str) = std::str::from_utf8(user_input) {
                                    if let Some(ghost) = completer.update(input_str) {
                                        let ghost_render = display::render_ghost_text(ghost);
                                        nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                                    }
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
                match action {
                    InterceptAction::Cancel => {
                        let dismiss = display::render_dismiss();
                        let restore = format!("\x1b[{}G", dismiss_col + 1);
                        nix::unistd::write(std::io::stdout(), dismiss.as_bytes()).ok();
                        nix::unistd::write(std::io::stdout(), restore.as_bytes()).ok();
                    }
                    InterceptAction::Forward(bytes) => {
                        // Bare ESC forwarded when not in chat mode
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

                    // Track cursor column on display (stripped) data
                    col_tracker.feed(display_data);

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
                                if let Some(title) = tmux_title("omnish", in_tmux) {
                                    nix::unistd::write(std::io::stdout(), title.as_bytes()).ok();
                                }
                            }
                            Osc133EventKind::CommandEnd { exit_code } => {
                                event_log::push(format!("osc133 CommandEnd exit_code={exit_code}"));
                                shell_input.on_prompt();
                                shell_completer.clear();
                                last_readline_content = None;
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
                                    send_ignored_summary(rpc, &mut shell_completer, &session_id, shell_cwd).await;
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
                                    send_ignored_summary(rpc, &mut shell_completer, &session_id, shell_cwd).await;
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
                                                let ghost_render = display::render_ghost_text(ghost);
                                                nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
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
                send_completion_summary(rpc, &mut shell_completer, &session_id, false, shell_cwd).await;
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
        let _ = rpc.call(msg).await;
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
            eprintln!("\x1b[33m[omnish]\x1b[0m Failed to load auth token: {}", e);
            eprintln!("\x1b[33m[omnish]\x1b[0m Running in passthrough mode (no daemon)");
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
                eprintln!("\x1b[33m[omnish]\x1b[0m Failed to set up TLS: {}", e);
                eprintln!("\x1b[33m[omnish]\x1b[0m Running in passthrough mode (no daemon)");
                return None;
            }
        }
    } else {
        None
    };

    match RpcClient::connect_with_reconnect(
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
                let auth_resp = rpc.call(Message::Auth(Auth { token })).await?;
                if matches!(auth_resp, Message::AuthFailed) {
                    anyhow::bail!("authentication failed");
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
    ).await {
        Ok(client) => {
            if client.is_connected().await {
                eprintln!("\x1b[32m[omnish]\x1b[0m Connected to daemon (session: {})", &session_id[..8]);
            } else {
                eprintln!("\x1b[33m[omnish]\x1b[0m Daemon not available, waiting for daemon to start...");
                eprintln!("\x1b[33m[omnish]\x1b[0m Socket: {}", socket_path);
                eprintln!("\x1b[33m[omnish]\x1b[0m To start daemon: omnish-daemon or cargo run -p omnish-daemon");
            }
            Some(client)
        }
        Err(e) => {
            // This should not happen with our updated connect_with_reconnect,
            // but keep for backward compatibility
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
fn debug_client_state(
    shell_input: &shell_input::ShellInputTracker,
    interceptor: &interceptor::InputInterceptor,
    shell_completer: &completion::ShellCompleter,
    daemon_conn: &Option<RpcClient>,
    _osc133_detector: &omnish_tracker::osc133_detector::Osc133Detector,
    last_readline: &Option<String>,
) -> String {
    let mut output = String::new();

    // Version info
    output.push_str(&format!("Version: omnish {}\n\n", omnish_common::VERSION));

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
    output.push_str(&format!("  sent_seq: {}\n", sent_seq));
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

/// Parse a daemon command response as JSON. Returns None if not valid JSON.
fn parse_cmd_response(content: &str) -> Option<serde_json::Value> {
    serde_json::from_str(content).ok()
}

/// Get the display string from a parsed command response JSON.
fn cmd_display_str(json: &serde_json::Value) -> String {
    json.get("display")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Display a command result or write to file if redirected.
fn handle_command_result(content: &str, redirect: Option<&str>, proxy: &PtyProxy) {
    if let Some(path) = redirect {
        // Resolve relative paths against shell's current working directory
        let resolved_path = if std::path::Path::new(path).is_relative() {
            match get_shell_cwd(proxy.child_pid() as u32) {
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
    // Clear bash readline before Enter so pre-chat input isn't executed (issue #24).
    proxy.write_all(b"\x15\x0b\r").ok();
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

    match rpc.call(request).await {
        Ok(Message::Response(resp)) if resp.request_id == request_id => {
            let display = if let Some(json) = parse_cmd_response(&resp.content) {
                cmd_display_str(&json)
            } else {
                resp.content.clone()
            };
            if show_thinking {
                std::fs::write("/tmp/omnish_last_response.txt", &display).ok();
            }
            handle_command_result(&display, redirect, proxy);
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
            proxy.write_all(b"\x15\x0b\r").ok();
        }
    }
}

/// Handle a /command in chat mode. Returns true if the command was handled.
async fn handle_slash_command(
    trimmed: &str,
    session_id: &str,
    rpc: &RpcClient,
    proxy: &PtyProxy,
    client_debug_fn: &dyn Fn() -> String,
) -> bool {
    match command::dispatch(trimmed) {
        command::ChatAction::Command { result, redirect, limit } => {
            let display_result = if let Some(ref l) = limit {
                command::apply_limit(&result, l)
            } else {
                result
            };
            if let Some(path) = redirect.as_deref() {
                handle_command_result(&display_result, Some(path), proxy);
            } else {
                let output = display::render_response(&display_result);
                nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
            }
            true
        }
        command::ChatAction::DaemonQuery { query, redirect, limit } => {
            // /debug client is intercepted client-side (needs local state)
            if query == "__cmd:client_debug" {
                let result = client_debug_fn();
                let output = display::render_response(&result);
                nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                return true;
            }
            if let Some(path) = redirect.as_deref() {
                send_daemon_query(&query, session_id, rpc, proxy, Some(path), false).await;
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
                        let output = display::render_response(&display);
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

/// Run the multi-turn chat loop. Returns when user exits via ESC, Ctrl-D, or backspace on empty input.
async fn run_chat_loop(
    rpc: &RpcClient,
    session_id: &str,
    proxy: &PtyProxy,
    initial_msg: Option<String>,
    client_debug_fn: &dyn Fn() -> String,
) {
    let mut chat_completer = ghost_complete::GhostCompleter::new(vec![
        Box::new(ghost_complete::BuiltinProvider::new()),
    ]);

    // Lazily created on first message or explicit /new
    let mut current_thread_id: Option<String> = None;
    // Cached thread_ids from last /conversations call, for stable /resume N
    let mut cached_thread_ids: Vec<String> = Vec::new();

    let mut pending_input = initial_msg;

    // Chat loop — LLM queries
    loop {
        let input = if let Some(msg) = pending_input.take() {
            msg
        } else {
            let prompt = display::render_chat_prompt();
            nix::unistd::write(std::io::stdout(), prompt.as_bytes()).ok();

            match read_chat_input(&mut chat_completer, true) {
                Some(line) => line,
                None => break, // ESC / Ctrl-D / backspace on empty
            }
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        // /new — start new thread within chat
        if trimmed == "/new" || trimmed == "/chat" || trimmed == "/ask" {
            let req_id = Uuid::new_v4().to_string()[..8].to_string();
            let new_msg = Message::ChatStart(ChatStart {
                request_id: req_id.clone(),
                session_id: session_id.to_string(),
                new_thread: true,
            });
            match rpc.call(new_msg).await {
                Ok(Message::ChatReady(ready)) if ready.request_id == req_id => {
                    current_thread_id = Some(ready.thread_id);
                    let info = "\r\n\x1b[2;37m(new conversation)\x1b[0m";
                    nix::unistd::write(std::io::stdout(), info.as_bytes()).ok();
                }
                _ => {
                    let err = display::render_error("Failed to create new thread");
                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                }
            }
            continue;
        }

        // /conversations — list and cache thread_ids for stable /resume N
        if trimmed == "/conversations" {
            let request_id = Uuid::new_v4().to_string()[..8].to_string();
            let request = Message::Request(Request {
                request_id: request_id.clone(),
                session_id: session_id.to_string(),
                query: "__cmd:conversations".to_string(),
                scope: RequestScope::AllSessions,
            });
            match rpc.call(request).await {
                Ok(Message::Response(resp)) if resp.request_id == request_id => {
                    if let Some(json) = parse_cmd_response(&resp.content) {
                        // Cache thread_ids for /resume N
                        if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                            cached_thread_ids = ids.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                        let display = cmd_display_str(&json);
                        let output = display::render_response(&display);
                        nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                    }
                }
                _ => {
                    let err = display::render_error("Failed to list conversations");
                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                }
            }
            continue;
        }

        // /resume [N] — switch to thread by cached index or latest
        if trimmed == "/resume" || trimmed.starts_with("/resume ") {
            // Resolve thread_id: from cache (if /resume N) or from daemon (if /resume)
            let (thread_id, display_msg) = if let Some(idx_str) = trimmed.strip_prefix("/resume ") {
                // Auto-fetch conversations if cache is empty
                if cached_thread_ids.is_empty() {
                    let rid = Uuid::new_v4().to_string()[..8].to_string();
                    let req = Message::Request(Request {
                        request_id: rid.clone(),
                        session_id: session_id.to_string(),
                        query: "__cmd:conversations".to_string(),
                        scope: RequestScope::AllSessions,
                    });
                    if let Ok(Message::Response(resp)) = rpc.call(req).await {
                        if resp.request_id == rid {
                            if let Some(json) = parse_cmd_response(&resp.content) {
                                if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                                    cached_thread_ids = ids.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect();
                                }
                            }
                        }
                    }
                }
                match idx_str.trim().parse::<usize>() {
                    Ok(i) if i >= 1 && i <= cached_thread_ids.len() => {
                        let tid = cached_thread_ids[i - 1].clone();
                        // Fetch last exchange display via daemon
                        let rid = Uuid::new_v4().to_string()[..8].to_string();
                        let req = Message::Request(Request {
                            request_id: rid.clone(),
                            session_id: session_id.to_string(),
                            query: format!("__cmd:resume {}", i),
                            scope: RequestScope::AllSessions,
                        });
                        let display = match rpc.call(req).await {
                            Ok(Message::Response(resp)) if resp.request_id == rid => {
                                parse_cmd_response(&resp.content)
                                    .map(|j| cmd_display_str(&j))
                            }
                            _ => None,
                        };
                        (Some(tid), display)
                    }
                    Ok(i) if i >= 1 => {
                        let err = display::render_error(&format!(
                            "Index {} out of range ({} conversations)",
                            i, cached_thread_ids.len()
                        ));
                        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        (None, None)
                    }
                    _ => {
                        let err = display::render_error("Invalid index");
                        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                        (None, None)
                    }
                }
            } else {
                // /resume without index — use latest via daemon
                let request_id = Uuid::new_v4().to_string()[..8].to_string();
                let request = Message::Request(Request {
                    request_id: request_id.clone(),
                    session_id: session_id.to_string(),
                    query: "__cmd:resume".to_string(),
                    scope: RequestScope::AllSessions,
                });
                match rpc.call(request).await {
                    Ok(Message::Response(resp)) if resp.request_id == request_id => {
                        if let Some(json) = parse_cmd_response(&resp.content) {
                            let tid = json.get("thread_id").and_then(|v| v.as_str()).map(String::from);
                            let display = Some(cmd_display_str(&json));
                            (tid, display)
                        } else {
                            (None, None)
                        }
                    }
                    _ => (None, None),
                }
            };
            if let Some(tid) = thread_id {
                current_thread_id = Some(tid);
                if let Some(msg) = display_msg {
                    let output = display::render_response(&msg);
                    nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                } else {
                    let info = "\r\n\x1b[2;37m(resumed conversation)\x1b[0m";
                    nix::unistd::write(std::io::stdout(), info.as_bytes()).ok();
                }
            }
            continue;
        }

        // /context in chat mode — show chat thread context
        if trimmed == "/context" {
            let query = if let Some(ref tid) = current_thread_id {
                format!("__cmd:context chat:{}", tid)
            } else {
                "__cmd:context".to_string()
            };
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
                    let output = display::render_response(&display);
                    nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                }
                _ => {
                    let err = display::render_error("Failed to get context");
                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                }
            }
            continue;
        }

        // Handle /commands that go through existing dispatch
        if trimmed.starts_with('/') {
            if handle_slash_command(trimmed, session_id, rpc, proxy, client_debug_fn).await {
                continue;
            }
            // Unknown /command — fall through to send as chat message
        }

        // Lazily create thread if not yet initialized
        if current_thread_id.is_none() {
            let req_id = Uuid::new_v4().to_string()[..8].to_string();
            let start_msg = Message::ChatStart(ChatStart {
                request_id: req_id.clone(),
                session_id: session_id.to_string(),
                new_thread: true,
            });
            match rpc.call(start_msg).await {
                Ok(Message::ChatReady(ready)) if ready.request_id == req_id => {
                    current_thread_id = Some(ready.thread_id);
                }
                _ => {
                    let err = display::render_error("Failed to start chat session");
                    nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                    continue;
                }
            }
        }

        // Show thinking indicator
        let thinking = display::render_thinking();
        nix::unistd::write(std::io::stdout(), thinking.as_bytes()).ok();

        // Send ChatMessage, allow Ctrl-C to interrupt
        let req_id = Uuid::new_v4().to_string()[..8].to_string();
        let chat_msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
            request_id: req_id.clone(),
            session_id: session_id.to_string(),
            thread_id: current_thread_id.clone().unwrap(),
            query: trimmed.to_string(),
        });

        // Race RPC call against Ctrl-C on stdin
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let interrupt = tokio::task::spawn_blocking(move || wait_for_ctrl_c(stop_rx));
        let rpc_result = rpc.call(chat_msg);

        tokio::select! {
            result = rpc_result => {
                // Signal the stdin reader to stop
                let _ = stop_tx.send(());
                match result {
                    Ok(Message::ChatResponse(resp)) if resp.request_id == req_id => {
                        // Clear thinking indicator
                        nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                        let output = display::render_response(&resp.content);
                        nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();

                        // Show separator
                        let (_rows, cols) = get_terminal_size().unwrap_or((24, 80));
                        let separator = display::render_separator(cols);
                        let sep_line = format!("{}\r\n", separator);
                        nix::unistd::write(std::io::stdout(), sep_line.as_bytes()).ok();
                    }
                    _ => {
                        nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                        let err = display::render_error("Failed to receive chat response");
                        nix::unistd::write(std::io::stdout(), err.as_bytes()).ok();
                    }
                }
            }
            _ = interrupt => {
                // Ctrl-C pressed — record interrupt in conversation
                nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();
                let info = "\r\n\x1b[2;37m(interrupted)\x1b[0m";
                nix::unistd::write(std::io::stdout(), info.as_bytes()).ok();

                // Send interrupt to daemon to record in conversation
                let interrupt_msg = Message::ChatInterrupt(omnish_protocol::message::ChatInterrupt {
                    session_id: session_id.to_string(),
                    thread_id: current_thread_id.clone().unwrap(),
                    query: trimmed.to_string(),
                });
                // Fire and forget — don't wait for response
                let rpc_clone = rpc.clone();
                tokio::spawn(async move {
                    let _ = rpc_clone.call(interrupt_msg).await;
                });
            }
        }
    }
}

/// Block until Ctrl-C (0x03) is read from stdin. Uses poll with 100ms timeout
/// so the thread exits promptly when `stop` is signalled.
fn wait_for_ctrl_c(stop: std::sync::mpsc::Receiver<()>) -> bool {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 1];
    loop {
        // Check if we should stop
        if stop.try_recv().is_ok() {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd: stdin_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 100) }; // 100ms timeout
        if ret <= 0 {
            continue;
        }
        match nix::unistd::read(stdin_fd, &mut byte) {
            Ok(1) if byte[0] == 0x03 => return true,
            Ok(1) => {} // Ignore other keys while waiting
            _ => return false,
        }
    }
}

/// Returns the byte length of the last UTF-8 character in the buffer.
/// Returns 1 if the buffer is empty or contains invalid UTF-8.
fn last_utf8_char_len(buf: &[u8]) -> usize {
    if buf.is_empty() {
        return 1;
    }
    // Walk backwards from the end, counting continuation bytes (10xx xxxx),
    // then +1 for the start byte.
    let mut cont = 0;
    for &b in buf.iter().rev() {
        if b & 0xC0 == 0x80 {
            cont += 1;
        } else {
            break;
        }
    }
    // cont continuation bytes + 1 start byte
    cont + 1
}

/// Read a line of input in raw mode for the chat loop.
/// Returns None on ESC, Ctrl-D, or backspace on empty input (if allow_backspace_exit is true).
fn read_chat_input(completer: &mut ghost_complete::GhostCompleter, allow_backspace_exit: bool) -> Option<String> {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    let mut has_ghost = false;

    loop {
        match nix::unistd::read(stdin_fd, &mut byte) {
            Ok(1) => {
                match byte[0] {
                    0x1b => return None,  // ESC — exit chat
                    0x04 if buf.is_empty() => return None,  // Ctrl-D on empty — exit chat
                    0x0d => {             // Enter — submit line
                        if has_ghost {
                            // Clear ghost text before submitting
                            nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                        }
                        completer.clear();
                        return Some(String::from_utf8_lossy(&buf).to_string());
                    }
                    0x09 => {             // Tab — accept ghost completion
                        if let Some(suffix) = completer.accept() {
                            buf.extend_from_slice(suffix.as_bytes());
                            // Clear ghost, rewrite accepted text in normal color
                            nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                            nix::unistd::write(std::io::stdout(), suffix.as_bytes()).ok();
                            has_ghost = false;
                            // Query for next ghost after accepting
                            let input = String::from_utf8_lossy(&buf);
                            if let Some(ghost) = completer.update(&input) {
                                let ghost_render = format!("\x1b[2;37m{}\x1b[0m\x1b[{}D", ghost, ghost.len());
                                nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                                has_ghost = true;
                            }
                        }
                    }
                    0x7f | 0x08 => {      // Backspace
                        if buf.is_empty() {
                            if allow_backspace_exit {
                                return None; // Backspace on empty — exit chat
                            }
                            // Otherwise, ignore the backspace (don't exit)
                        }
                        // Get the last UTF-8 character bytes BEFORE removing
                        let last_char_len = last_utf8_char_len(&buf);
                        let deleted_bytes = buf[buf.len().saturating_sub(last_char_len)..].to_vec();
                        // Remove the last UTF-8 character (not just 1 byte)
                        for _ in 0..last_char_len {
                            buf.pop();
                        }
                        // Calculate visual width of deleted character for erasing
                        let erase_width = String::from_utf8_lossy(&deleted_bytes).chars().next()
                            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
                            .unwrap_or(1);
                        // Erase character: move cursor back by visual width + clear to end of line
                        let backspaces = "\x08".repeat(erase_width);
                        let erase_seq = format!("{}\x1b[K", backspaces);
                        nix::unistd::write(std::io::stdout(), erase_seq.as_bytes()).ok();
                        has_ghost = false;
                        // Update ghost for new input
                        let input = String::from_utf8_lossy(&buf);
                        if let Some(ghost) = completer.update(&input) {
                            let ghost_render = format!("\x1b[2;37m{}\x1b[0m\x1b[{}D", ghost, ghost.len());
                            nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                            has_ghost = true;
                        } else {
                            completer.clear();
                        }
                    }
                    b if b >= 0x20 => {   // Printable ASCII or UTF-8 lead byte
                        buf.push(b);
                        // Clear any existing ghost text, write char
                        if has_ghost {
                            nix::unistd::write(std::io::stdout(), b"\x1b[K").ok();
                            has_ghost = false;
                        }
                        nix::unistd::write(std::io::stdout(), &[b]).ok();
                        // Handle UTF-8 continuation bytes
                        if b >= 0xC0 {
                            let expected = if b < 0xE0 { 1 } else if b < 0xF0 { 2 } else { 3 };
                            for _ in 0..expected {
                                if nix::unistd::read(stdin_fd, &mut byte).unwrap_or(0) == 1 {
                                    buf.push(byte[0]);
                                    nix::unistd::write(std::io::stdout(), &byte).ok();
                                }
                            }
                        }
                        // Update ghost completion
                        let input = String::from_utf8_lossy(&buf);
                        if let Some(ghost) = completer.update(&input) {
                            let ghost_render = format!("\x1b[2;37m{}\x1b[0m\x1b[{}D", ghost, ghost.len());
                            nix::unistd::write(std::io::stdout(), ghost_render.as_bytes()).ok();
                            has_ghost = true;
                        } else {
                            completer.clear();
                        }
                    }
                    _ => {}               // Ignore other control chars
                }
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_last_utf8_char_len_ascii() {
        assert_eq!(last_utf8_char_len(b"hello"), 1);
    }

    #[test]
    fn test_last_utf8_char_len_chinese() {
        // '你' = 0xe4 0xbd 0xa0 (3 bytes)
        assert_eq!(last_utf8_char_len("你".as_bytes()), 3);
        assert_eq!(last_utf8_char_len("hello你".as_bytes()), 3);
    }

    #[test]
    fn test_last_utf8_char_len_emoji() {
        // '😀' = 0xf0 0x9f 0x98 0x80 (4 bytes)
        assert_eq!(last_utf8_char_len("😀".as_bytes()), 4);
    }

    #[test]
    fn test_last_utf8_char_len_two_byte() {
        // 'é' = 0xc3 0xa9 (2 bytes)
        assert_eq!(last_utf8_char_len("é".as_bytes()), 2);
    }

    #[test]
    fn test_last_utf8_char_len_empty() {
        assert_eq!(last_utf8_char_len(b""), 1);
    }

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

        // Normal mode: ":" matches prefix immediately → Chat("")
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Chat(String::new()));

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

        // Back to normal: ":" should intercept again → Chat("")
        assert_eq!(interceptor.feed_byte(b':'), InterceptAction::Chat(String::new()));
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
}
