//! Bubblewrap (bwrap) sandbox backend (Linux only).

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

pub fn is_available() -> bool {
    which::which("bwrap").is_ok()
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
