use anyhow::Result;
use nix::sys::termios::{self, SetArg, Termios};
use std::os::fd::{BorrowedFd, RawFd};

pub struct RawModeGuard {
    fd: RawFd,
    original: Termios,
}

impl RawModeGuard {
    pub fn enter(fd: RawFd) -> Result<Self> {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let original = termios::tcgetattr(&borrowed)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(&borrowed, SetArg::TCSANOW, &raw)?;
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = termios::tcsetattr(&borrowed, SetArg::TCSANOW, &self.original);
    }
}
