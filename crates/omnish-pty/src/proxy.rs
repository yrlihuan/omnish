use anyhow::{Context, Result};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{dup2, execvp, fork, read, write, setsid, ForkResult, Pid};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub struct PtyProxy {
    master_fd: OwnedFd,
    child_pid: Pid,
}

impl PtyProxy {
    pub fn spawn(cmd: &str, args: &[&str]) -> Result<Self> {
        Self::spawn_with_env(cmd, args, HashMap::new())
    }

    pub fn spawn_with_env(cmd: &str, args: &[&str], env: HashMap<String, String>) -> Result<Self> {
        let OpenptyResult { master, slave } =
            openpty(None, None).context("openpty failed")?;

        match unsafe { fork() }.context("fork failed")? {
            ForkResult::Child => {
                drop(master);

                setsid().ok();
                unsafe {
                    libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
                }

                dup2(slave.as_raw_fd(), 0).ok();
                dup2(slave.as_raw_fd(), 1).ok();
                dup2(slave.as_raw_fd(), 2).ok();
                if slave.as_raw_fd() > 2 {
                    drop(slave);
                }

                // Set environment variables in the child before exec
                for (key, value) in &env {
                    std::env::set_var(key, value);
                }

                let c_cmd = CString::new(cmd).unwrap();
                let mut c_args: Vec<CString> = vec![c_cmd.clone()];
                for a in args {
                    c_args.push(CString::new(*a).unwrap());
                }
                execvp(&c_cmd, &c_args).ok();
                unsafe { libc::_exit(127) };
            }
            ForkResult::Parent { child } => {
                drop(slave);
                Ok(PtyProxy {
                    master_fd: master,
                    child_pid: child,
                })
            }
        }
    }

    /// Reconstruct a PtyProxy from an existing master fd and child pid.
    /// Used for resuming after exec (the fd and child survive the exec boundary).
    ///
    /// # Safety
    /// The caller must ensure `fd` is a valid open PTY master file descriptor
    /// and `pid` is a valid child process ID.
    pub unsafe fn from_raw_fd(fd: RawFd, pid: i32) -> Self {
        PtyProxy {
            master_fd: OwnedFd::from_raw_fd(fd),
            child_pid: Pid::from_raw(pid),
        }
    }

    pub fn master_raw_fd(&self) -> RawFd {
        self.master_fd.as_raw_fd()
    }

    pub fn child_pid(&self) -> i32 {
        self.child_pid.as_raw()
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let n = read(self.master_fd.as_raw_fd(), buf)
            .context("read from PTY master")?;
        Ok(n)
    }

    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut written = 0;
        while written < data.len() {
            let n = write(&self.master_fd, &data[written..])
                .context("write to PTY master")?;
            written += n;
        }
        Ok(())
    }

    pub fn set_window_size(&self, rows: u16, cols: u16) -> Result<()> {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret = unsafe {
            libc::ioctl(self.master_fd.as_raw_fd(), libc::TIOCSWINSZ, &ws)
        };
        if ret < 0 {
            anyhow::bail!("ioctl TIOCSWINSZ failed");
        }
        Ok(())
    }

    pub fn wait(&self) -> Result<i32> {
        match waitpid(self.child_pid, None)? {
            WaitStatus::Exited(_, code) => Ok(code),
            WaitStatus::Signaled(_, sig, _) => Ok(128 + sig as i32),
            _ => Ok(-1),
        }
    }

    /// Kill the current child and spawn a new shell in a fresh PTY.
    /// Returns the new master_fd raw value (caller must update poll fds and SIGWINCH).
    /// An optional `pre_exec` closure runs in the child before exec (e.g. for Landlock).
    pub fn respawn(
        &mut self,
        cmd: &str,
        args: &[&str],
        env: HashMap<String, String>,
        cwd: Option<&std::path::Path>,
        pre_exec: Option<Box<dyn FnOnce() -> Result<(), String> + Send>>,
    ) -> Result<RawFd> {
        // Kill old child
        nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGKILL).ok();
        // Reap zombie (non-blocking — child might already be gone)
        waitpid(self.child_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)).ok();

        // Create new PTY pair
        let OpenptyResult { master, slave } =
            openpty(None, None).context("openpty failed (respawn)")?;

        match unsafe { fork() }.context("fork failed (respawn)")? {
            ForkResult::Child => {
                drop(master);

                setsid().ok();
                unsafe {
                    libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
                }

                dup2(slave.as_raw_fd(), 0).ok();
                dup2(slave.as_raw_fd(), 1).ok();
                dup2(slave.as_raw_fd(), 2).ok();
                if slave.as_raw_fd() > 2 {
                    drop(slave);
                }

                // Set cwd
                if let Some(dir) = cwd {
                    std::env::set_current_dir(dir).ok();
                }

                // Set environment variables
                for (key, value) in &env {
                    std::env::set_var(key, value);
                }

                // Apply pre_exec (e.g. Landlock sandbox)
                if let Some(f) = pre_exec {
                    if let Err(e) = f() {
                        let msg = format!("pre_exec failed: {}\n", e);
                        nix::unistd::write(std::io::stderr(), msg.as_bytes()).ok();
                        unsafe { libc::_exit(126) };
                    }
                }

                let c_cmd = CString::new(cmd).unwrap();
                let mut c_args: Vec<CString> = vec![c_cmd.clone()];
                for a in args {
                    c_args.push(CString::new(*a).unwrap());
                }
                execvp(&c_cmd, &c_args).ok();
                unsafe { libc::_exit(127) };
            }
            ForkResult::Parent { child } => {
                drop(slave);
                let new_fd = master.as_raw_fd();
                self.master_fd = master;
                self.child_pid = child;
                Ok(new_fd)
            }
        }
    }
}
