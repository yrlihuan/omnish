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
    // Reject paths with control characters (newlines etc.) that would break .sb profile syntax
    assert!(
        !path.bytes().any(|b| b < 0x20),
        "sandbox profile path contains control characters: {path:?}"
    );
    path.replace('\\', "\\\\").replace('"', "\\\"")
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
