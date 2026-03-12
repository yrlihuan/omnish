//! Shared plugin infrastructure: Landlock sandbox and tool implementations.

pub mod tools;

#[cfg(target_os = "linux")]
use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};

// --- Landlock sandbox (Linux only) ---

/// Apply Landlock filesystem sandbox: read everywhere, write only to `data_dir`, `/tmp`,
/// and optionally the current working directory.
/// Called inside `pre_exec` (between fork and exec), so only affects the child process.
#[cfg(target_os = "linux")]
pub fn apply_sandbox(data_dir: &std::path::Path, cwd: Option<&std::path::Path>) -> Result<(), String> {
    let abi = ABI::V1;

    // Build writable paths: data_dir, /tmp, and optionally cwd
    let mut writable_paths = vec![data_dir, std::path::Path::new("/tmp")];
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
