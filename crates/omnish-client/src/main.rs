// crates/omnish-client/src/main.rs
mod commands;
mod interceptor;

use anyhow::Result;
use commands::{parse_command, OmnishCommand};
use interceptor::{InputInterceptor, InterceptAction};
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
    let mut interceptor = InputInterceptor::new("::");

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
            let mut i = 0;
            while i < n {
                let byte = input_buf[i];
                match interceptor.feed_byte(byte) {
                    InterceptAction::PassThrough => {
                        // Normal pass-through
                        i += 1;
                    }
                    InterceptAction::Command(cmd_str, _buffered) => {
                        // Command detected - handle it
                        if let Some(ref conn) = daemon_conn {
                            handle_command(&cmd_str, &session_id, conn).await;
                        } else {
                            // No daemon connection, print error
                            let err_msg = "\r\n[omnish] Daemon not connected, command ignored\r\n";
                            nix::unistd::write(std::io::stdout(), err_msg.as_bytes()).ok();
                        }
                        // Skip the buffered command bytes
                        i = n;
                        break;
                    }
                }
            }

            // Pass through to PTY (excluding any consumed command bytes)
            if i > 0 {
                proxy.write_all(&input_buf[..i])?;

                // Report to daemon async
                if let Some(ref conn) = daemon_conn {
                    let msg = Message::IoData(IoData {
                        session_id: session_id.clone(),
                        direction: IoDirection::Input,
                        timestamp_ms: timestamp_ms(),
                        data: input_buf[..i].to_vec(),
                    });
                    let _ = conn.send(&msg).await;
                }
            }
        }

        // PTY master -> stdout
        if fds[1].revents & libc::POLLIN != 0 {
            match proxy.read(&mut output_buf) {
                Ok(0) => break,
                Ok(n) => {
                    nix::unistd::write(std::io::stdout(), &output_buf[..n])?;

                    // Notify interceptor of output (resets command state)
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
                Some(conn)
            } else {
                None
            }
        }
        Err(_) => {
            eprintln!("[omnish] daemon not available, running in passthrough mode");
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

async fn handle_command(cmd_str: &str, session_id: &str, conn: &Box<dyn Connection>) {
    let cmd = parse_command(&format!("::{}", cmd_str), "::");

    match cmd {
        Some(OmnishCommand::Ask { flags, query }) => {
            // Clear the command line
            nix::unistd::write(std::io::stdout(), b"\r\x1b[K").ok();

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
                nix::unistd::write(std::io::stdout(), b"\r\n[omnish] Failed to send request\r\n").ok();
                return;
            }

            // Wait for response
            match conn.recv().await {
                Ok(Message::Response(resp)) if resp.request_id == request_id => {
                    let output = format!("\r\n[omnish] {}\r\n", resp.content);
                    nix::unistd::write(std::io::stdout(), output.as_bytes()).ok();
                }
                Ok(_) => {
                    nix::unistd::write(std::io::stdout(), b"\r\n[omnish] Unexpected response\r\n").ok();
                }
                Err(_) => {
                    nix::unistd::write(std::io::stdout(), b"\r\n[omnish] Failed to receive response\r\n").ok();
                }
            }
        }
        Some(OmnishCommand::Unknown(s)) => {
            let msg = format!("\r\n[omnish] Unknown command: {}\r\n", s);
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
        }
        _ => {
            nix::unistd::write(std::io::stdout(), b"\r\n[omnish] Command not yet implemented\r\n").ok();
        }
    }
}
