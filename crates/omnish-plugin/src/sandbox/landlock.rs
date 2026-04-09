//! Landlock sandbox backend (Linux only).

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// Check whether the running kernel supports Landlock (>= 5.13).
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    detect_abi().is_some()
}

#[cfg(not(target_os = "linux"))]
pub fn is_available() -> bool {
    false
}

/// Detect the best Landlock ABI supported by the running kernel.
/// Returns `None` if Landlock is not available (kernel < 5.13).
///
/// ABI versions and minimum kernel requirements:
///   V1 = 5.13, V2 = 5.19, V3 = 6.2, V4 = 6.7, V5 = 6.10
#[cfg(target_os = "linux")]
fn detect_abi() -> Option<landlock::ABI> {
    let mut utsname: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut utsname) } != 0 {
        return None;
    }
    let release = unsafe { std::ffi::CStr::from_ptr(utsname.release.as_ptr()) };
    let release = release.to_str().ok()?;
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;

    let ver = (major, minor);
    if ver >= (6, 10) {
        Some(landlock::ABI::V5)
    } else if ver >= (6, 7) {
        Some(landlock::ABI::V4)
    } else if ver >= (6, 2) {
        Some(landlock::ABI::V3)
    } else if ver >= (5, 19) {
        Some(landlock::ABI::V2)
    } else if ver >= (5, 13) {
        Some(landlock::ABI::V1)
    } else {
        None
    }
}

/// Core Landlock enforcement: read everywhere, write only to the given paths.
/// Called inside `pre_exec` (between fork and exec), so only affects the child process.
/// Skips sandboxing on kernels that do not support Landlock (< 5.13).
#[cfg(target_os = "linux")]
fn apply_landlock(writable_paths: &[&Path]) -> Result<(), String> {
    use landlock::{
        path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus,
    };

    let abi = match detect_abi() {
        Some(abi) => abi,
        None => return Ok(()), // kernel too old for Landlock
    };
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| format!("landlock handle_access: {e}"))?
        .create()
        .map_err(|e| format!("landlock create: {e}"))?
        .add_rules(path_beneath_rules(&["/"], AccessFs::from_read(abi)))
        .map_err(|e| format!("landlock add read rules: {e}"))?
        .add_rules(path_beneath_rules(writable_paths, AccessFs::from_all(abi)))
        .map_err(|e| format!("landlock add write rules: {e}"))?
        .restrict_self()
        .map_err(|e| format!("landlock restrict_self: {e}"))?;
    match status.ruleset {
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => Ok(()),
        RulesetStatus::NotEnforced => Ok(()),
    }
}

/// Build a sandboxed Command that applies Landlock via pre_exec.
#[cfg(target_os = "linux")]
pub fn sandbox_command(
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    use std::os::unix::process::CommandExt;

    let writable: Vec<std::path::PathBuf> = policy.writable_paths.clone();
    let mut cmd = Command::new(executable);
    cmd.args(args);

    unsafe {
        cmd.pre_exec(move || {
            let refs: Vec<&Path> = writable.iter().map(|p| p.as_path()).collect();
            apply_landlock(&refs).map_err(std::io::Error::other)
        });
    }

    Ok(cmd)
}

#[cfg(not(target_os = "linux"))]
pub fn sandbox_command(
    _policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    let mut cmd = Command::new(executable);
    cmd.args(args);
    Ok(cmd)
}

/// Apply Landlock restrictions in the current process from a SandboxPolicy.
/// Used by handle_lock and other contexts where we can't wrap the command.
#[cfg(target_os = "linux")]
pub fn apply_landlock_from_policy(policy: &SandboxPolicy) -> Result<(), String> {
    let refs: Vec<&Path> = policy.writable_paths.iter().map(|p| p.as_path()).collect();
    apply_landlock(&refs)
}

#[cfg(not(target_os = "linux"))]
pub fn apply_landlock_from_policy(_policy: &SandboxPolicy) -> Result<(), String> {
    Ok(())
}
