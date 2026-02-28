use anyhow::{Context, Result};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{dup2, execvp, fork, read, write, setsid, ForkResult, Pid};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

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

    pub fn master_raw_fd(&self) -> RawFd {
        self.master_fd.as_raw_fd()
    }

    pub fn child_pid(&self) -> i32 {
        self.child_pid.as_raw() as i32
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
}
