//! Bubblewrap (bwrap) sandbox backend (Linux only).

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// Why bwrap is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BwrapUnavailableReason {
    /// bwrap binary not found.
    NotInstalled,
    /// bwrap exists but cannot create user namespaces (e.g. AppArmor restriction).
    NamespaceDenied,
}

pub fn is_available() -> bool {
    unavailable_reason().is_none()
}

/// Returns `None` if bwrap is available, or the reason it is not.
pub fn unavailable_reason() -> Option<BwrapUnavailableReason> {
    if which::which("bwrap").is_err() {
        return Some(BwrapUnavailableReason::NotInstalled);
    }
    // Probe whether bwrap can actually create namespaces.
    // On systems with AppArmor restricting unprivileged user namespaces,
    // bwrap fails at runtime with "setting up uid map: Permission denied".
    let ok = std::process::Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--", "/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        None
    } else {
        Some(BwrapUnavailableReason::NamespaceDenied)
    }
}

pub fn sandbox_command(
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    let mut cmd = Command::new("bwrap");
    cmd.args(["--new-session", "--die-with-parent"]);
    cmd.args(["--ro-bind", "/", "/"]);
    cmd.args(["--dev", "/dev"]);

    for path in &policy.writable_paths {
        let path_str = path.to_string_lossy();
        if path_str.starts_with("/dev/") || path_str == "/dev" {
            continue;
        }
        if path.exists() {
            cmd.args(["--bind", &path_str, &path_str]);
        }
    }

    for path in &policy.deny_read {
        if !path.exists() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if path.is_dir() {
            cmd.args(["--tmpfs", &path_str]);
        } else {
            cmd.args(["--ro-bind", "/dev/null", &path_str]);
        }
    }

    cmd.arg("--");
    cmd.arg(executable);
    cmd.args(args);

    Ok(cmd)
}
