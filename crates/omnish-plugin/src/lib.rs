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

/// Escape a path string for use inside a sandbox-exec `.sb` profile.
/// Escapes backslashes first, then double quotes, to prevent profile injection.
#[cfg(any(target_os = "macos", test))]
fn escape_sb_path(path: &str) -> String {
    // Strip control characters (newlines, tabs, etc.) that would break .sb profile syntax,
    // then escape backslashes and quotes
    let cleaned: String = path.chars().filter(|c| !c.is_control()).collect();
    cleaned.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build a sandbox-exec `.sb` profile string.
/// Policy: deny all by default, allow all non-file-write operations,
/// allow file reads everywhere, allow file writes only to specified paths.
#[cfg(any(target_os = "macos", test))]
fn build_sandbox_profile(
    data_dir: &std::path::Path,
    cwd: Option<&std::path::Path>,
    repo_root: Option<&std::path::Path>,
) -> String {
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow signal)\n\
         (allow sysctl*)\n\
         (allow mach*)\n\
         (allow ipc*)\n\
         (allow network*)\n\
         (allow file-read*)\n\
         (allow file-write* (subpath \"/tmp\"))\n\
         (allow file-write* (literal \"/dev/null\"))\n",
    );

    let escaped = escape_sb_path(&data_dir.to_string_lossy());
    profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));

    if let Some(cwd) = cwd {
        let escaped = escape_sb_path(&cwd.to_string_lossy());
        profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
    }

    if let Some(root) = repo_root {
        // Avoid duplicate rule when repo root is the same as cwd
        if Some(root) != cwd {
            let escaped = escape_sb_path(&root.to_string_lossy());
            profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
        }
    }

    profile
}

/// Build a sandbox-exec `.sb` profile for macOS.
/// Computes git repo root from `cwd` (if provided) and delegates to `build_sandbox_profile()`.
#[cfg(target_os = "macos")]
pub fn sandbox_profile(data_dir: &std::path::Path, cwd: Option<&std::path::Path>) -> String {
    let repo_root = cwd.and_then(git_repo_root);
    build_sandbox_profile(data_dir, cwd, repo_root.as_deref())
}

/// Core Landlock enforcement: read everywhere, write only to the given paths.
/// Called inside `pre_exec` (between fork and exec), so only affects the child process.
#[cfg(target_os = "linux")]
fn apply_landlock(writable_paths: &[&std::path::Path]) -> Result<(), String> {
    let abi = ABI::V1;
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
        RulesetStatus::NotEnforced => Err("Landlock not supported on this kernel".into()),
    }
}

/// Build the common writable paths: /tmp, /dev/null, cwd, and git repo root.
fn common_writable_paths(cwd: Option<&std::path::Path>) -> (Vec<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let mut paths = vec![
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/dev/null"),
    ];
    let repo_root = cwd.and_then(git_repo_root);
    if let Some(ref root) = repo_root {
        paths.push(root.clone());
    }
    if let Some(cwd) = cwd {
        paths.push(cwd.to_path_buf());
    }
    (paths, repo_root)
}

/// Apply Landlock filesystem sandbox for plugins: read everywhere, write only to
/// `data_dir`, `/tmp`, `/dev/null`, cwd, and git repo root.
#[cfg(target_os = "linux")]
pub fn apply_sandbox(data_dir: &std::path::Path, cwd: Option<&std::path::Path>) -> Result<(), String> {
    let (mut paths, _) = common_writable_paths(cwd);
    paths.insert(0, data_dir.to_path_buf());
    let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    apply_landlock(&refs)
}

/// Apply Landlock filesystem sandbox for `/lock` mode: read everywhere, write only to
/// `/tmp`, `/dev/null`, cwd, and git repo root. No plugin data_dir.
#[cfg(target_os = "linux")]
pub fn apply_lock_sandbox(cwd: Option<&std::path::Path>) -> Result<(), String> {
    let (paths, _) = common_writable_paths(cwd);
    let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    apply_landlock(&refs)
}

/// No-op: on macOS, sandboxing is applied at the command level via sandbox-exec.
/// On other non-Linux platforms, sandboxing is not available.
#[cfg(not(target_os = "linux"))]
pub fn apply_sandbox(_data_dir: &std::path::Path, _cwd: Option<&std::path::Path>) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn apply_lock_sandbox(_cwd: Option<&std::path::Path>) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_escape_sb_path_no_special_chars() {
        assert_eq!(escape_sb_path("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn test_escape_sb_path_with_quotes() {
        assert_eq!(escape_sb_path("/path/with\"quote"), "/path/with\\\"quote");
    }

    #[test]
    fn test_escape_sb_path_with_backslash() {
        assert_eq!(escape_sb_path("/path/with\\slash"), "/path/with\\\\slash");
    }

    #[test]
    fn test_escape_sb_path_backslash_before_quote() {
        assert_eq!(escape_sb_path("a\\\"b"), "a\\\\\\\"b");
    }

    #[test]
    fn test_build_sandbox_profile_minimal() {
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            None,
            None,
        );
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp\"))"));
        assert!(profile.contains("(allow file-write* (literal \"/dev/null\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/data/plugin\"))"));
        assert_eq!(profile.matches("(allow file-write*").count(), 3);
    }

    #[test]
    fn test_build_sandbox_profile_with_cwd_and_same_repo() {
        // When cwd == repo_root, only one rule is emitted (deduplication)
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            Some(Path::new("/home/user/project")),
            Some(Path::new("/home/user/project")),
        );
        assert!(profile.contains("(allow file-write* (subpath \"/home/user/project\"))"));
        // data_dir + /tmp + /dev/null + cwd = 4 (repo deduped)
        assert_eq!(profile.matches("(allow file-write*").count(), 4);
    }

    #[test]
    fn test_build_sandbox_profile_with_cwd_and_different_repo() {
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            Some(Path::new("/home/user/project/subdir")),
            Some(Path::new("/home/user/project")),
        );
        assert!(profile.contains("(allow file-write* (subpath \"/home/user/project/subdir\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/home/user/project\"))"));
        // data_dir + /tmp + /dev/null + cwd + repo = 5
        assert_eq!(profile.matches("(allow file-write*").count(), 5);
    }

    #[test]
    fn test_build_sandbox_profile_with_cwd_only() {
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            Some(Path::new("/work")),
            None,
        );
        assert_eq!(profile.matches("(allow file-write*").count(), 4);
        assert!(profile.contains("(allow file-write* (subpath \"/work\"))"));
    }
}
