//! Shared plugin infrastructure: Landlock sandbox and tool implementations.

pub mod formatter;
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
    // Use (allow default) + deny file-write + selective allow file-write.
    // This matches Landlock semantics: restrict only filesystem writes, allow everything else.
    // With (deny default) we had to enumerate every sandbox operation class (process*, sysctl*,
    // mach*, etc.) and kept missing operations needed by tools like ps/top.
    // In Apple's sandbox evaluation, a more specific subpath wins over a less specific one,
    // so (allow file-write* (subpath "/tmp")) overrides (deny file-write* (subpath "/")).
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (allow sysctl-read)\n\
         (deny file-write* (subpath \"/\"))\n\
         (allow file-write* (subpath \"/tmp\"))\n\
         (allow file-write* (literal \"/dev/null\"))\n\
         (allow file-write* (subpath \"/opt/homebrew\"))\n",
    );

    let escaped = escape_sb_path(&data_dir.to_string_lossy());
    profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));

    // Well-known dotdirs
    if let Some(home) = dirs::home_dir() {
        for name in &[".ssh", ".cargo", ".config", ".local", ".claude", ".omnish", ".cache", ".npm", ".rustup", ".gnupg", ".docker", ".kube", ".nvm", ".pyenv"] {
            let escaped = escape_sb_path(&home.join(name).to_string_lossy());
            profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
        }
    }

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
    let abi = ABI::V5;
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

/// Build the common writable paths: /tmp, /dev/null, cwd, git repo root,
/// and well-known user dotdirs (~/.ssh, ~/.cargo, ~/.config, etc.).
#[cfg(target_os = "linux")]
fn common_writable_paths(cwd: Option<&std::path::Path>) -> (Vec<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let mut paths: Vec<std::path::PathBuf> = [
        "/tmp", "/dev/null", "/home/linuxbrew/.linuxbrew", "/var/spool/cron",
    ].iter().map(std::path::PathBuf::from).collect();

    // Well-known dotdirs that system commands commonly write to
    if let Some(home) = dirs::home_dir() {
        for name in &[".ssh", ".cargo", ".config", ".local", ".claude", ".omnish", ".cache", ".npm", ".rustup", ".gnupg", ".docker", ".kube", ".nvm", ".pyenv"] {
            paths.push(home.join(name));
        }
    }

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
        assert!(profile.contains("(allow default)"));
        assert!(profile.contains("(deny file-write* (subpath \"/\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp\"))"));
        assert!(profile.contains("(allow file-write* (literal \"/dev/null\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/data/plugin\"))"));
        // At least: /tmp + /dev/null + data_dir = 3, plus any existing dotdirs
        assert!(profile.matches("(allow file-write*").count() >= 3);
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
        // At least: data_dir + /tmp + /dev/null + cwd = 4
        assert!(profile.matches("(allow file-write*").count() >= 4);
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
        // At least: data_dir + /tmp + /dev/null + cwd + repo = 5
        assert!(profile.matches("(allow file-write*").count() >= 5);
    }

    #[test]
    fn test_build_sandbox_profile_with_cwd_only() {
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            Some(Path::new("/work")),
            None,
        );
        assert!(profile.matches("(allow file-write*").count() >= 4);
        assert!(profile.contains("(allow file-write* (subpath \"/work\"))"));
    }
}
