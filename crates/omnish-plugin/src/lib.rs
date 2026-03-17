//! Shared plugin infrastructure: Landlock sandbox and tool implementations.

pub mod tools;

#[cfg(target_os = "linux")]
use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};

// --- Landlock sandbox (Linux only) ---

/// Detect the git repository root for a given directory.
/// Returns `None` if the directory is not inside a git repo.
fn git_repo_root(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

/// Apply Landlock filesystem sandbox: read everywhere, write only to `data_dir`, `/tmp`,
/// and optionally the current working directory (+ git repo root if inside a repo).
/// Called inside `pre_exec` (between fork and exec), so only affects the child process.
#[cfg(target_os = "linux")]
pub fn apply_sandbox(data_dir: &std::path::Path, cwd: Option<&std::path::Path>) -> Result<(), String> {
    let abi = ABI::V1;

    // Build writable paths: data_dir, /tmp, /dev/null, and optionally cwd + git repo root
    // /dev/null is needed by many programs (e.g. git) for output redirection
    let mut writable_paths: Vec<&std::path::Path> = vec![
        data_dir,
        std::path::Path::new("/tmp"),
        std::path::Path::new("/dev/null"),
    ];
    let repo_root = cwd.and_then(git_repo_root);
    if let Some(ref root) = repo_root {
        writable_paths.push(root.as_path());
    }
    if let Some(cwd) = cwd {
        writable_paths.push(cwd);
    }

    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| format!("landlock handle_access: {e}"))?
        .create()
        .map_err(|e| format!("landlock create: {e}"))?
        .add_rules(path_beneath_rules(&["/"], AccessFs::from_read(abi)))
        .map_err(|e| format!("landlock add read rules: {e}"))?
        .add_rules(path_beneath_rules(&writable_paths, AccessFs::from_all(abi)))
        .map_err(|e| format!("landlock add write rules: {e}"))?
        .restrict_self()
        .map_err(|e| format!("landlock restrict_self: {e}"))?;
    match status.ruleset {
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => Ok(()),
        RulesetStatus::NotEnforced => Err("Landlock not supported on this kernel".into()),
    }
}

/// No-op sandbox on non-Linux platforms.
#[cfg(not(target_os = "linux"))]
pub fn apply_sandbox(_data_dir: &std::path::Path, _cwd: Option<&std::path::Path>) -> Result<(), String> {
    Ok(())
}
