//! macOS sandbox-exec (seatbelt) sandbox backend.

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// Escape a path string for use inside a sandbox-exec `.sb` profile.
/// Escapes backslashes first, then double quotes, to prevent profile injection.
fn escape_sb_path(path: &str) -> String {
    // Strip control characters (newlines, tabs, etc.) that would break .sb profile syntax,
    // then escape backslashes and quotes
    let cleaned: String = path.chars().filter(|c| !c.is_control()).collect();
    cleaned.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build a sandbox-exec `.sb` profile from a SandboxPolicy.
/// Policy: deny all writes by default, allow reads everywhere,
/// allow writes only to specified paths.
fn build_profile(policy: &SandboxPolicy) -> String {
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (allow sysctl-read)\n\
         (deny file-write* (subpath \"/\"))\n",
    );

    for path in &policy.writable_paths {
        let path_str = path.to_string_lossy();
        let escaped = escape_sb_path(&path_str);
        // Use (literal ...) for specific device files, (subpath ...) for directories
        if path_str == "/dev/null" || path_str == "/dev/tty" {
            profile.push_str(&format!("(allow file-write* (literal \"{escaped}\"))\n"));
        } else {
            profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
        }
    }

    profile
}

/// Build a sandbox-exec `.sb` profile string from a SandboxPolicy.
/// Public helper for backward compatibility.
pub fn build_profile_from_policy(policy: &SandboxPolicy) -> String {
    build_profile(policy)
}

/// Build a sandboxed Command using macOS sandbox-exec.
#[cfg(target_os = "macos")]
pub fn sandbox_command(
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    let profile = build_profile(policy);
    let mut cmd = Command::new("sandbox-exec");
    cmd.args(["-p", &profile]);
    cmd.arg(executable);
    cmd.args(args);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        assert_eq!(
            escape_sb_path("/path/with\\slash"),
            "/path/with\\\\slash"
        );
    }

    #[test]
    fn test_escape_sb_path_backslash_before_quote() {
        assert_eq!(escape_sb_path("a\\\"b"), "a\\\\\\\"b");
    }

    #[test]
    fn test_build_profile_minimal() {
        let policy = SandboxPolicy {
            writable_paths: vec![
                PathBuf::from("/tmp"),
                PathBuf::from("/dev/null"),
                PathBuf::from("/data/plugin"),
            ],
            deny_read: Vec::new(),
            allow_network: true,
        };
        let profile = build_profile(&policy);
        assert!(profile.contains("(allow default)"));
        assert!(profile.contains("(deny file-write* (subpath \"/\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp\"))"));
        assert!(profile.contains("(allow file-write* (literal \"/dev/null\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/data/plugin\"))"));
        assert!(profile.matches("(allow file-write*").count() >= 3);
    }

    #[test]
    fn test_build_profile_with_cwd_and_data() {
        let policy = SandboxPolicy {
            writable_paths: vec![
                PathBuf::from("/data/plugin"),
                PathBuf::from("/tmp"),
                PathBuf::from("/dev/null"),
                PathBuf::from("/dev/tty"),
                PathBuf::from("/home/user/project"),
            ],
            deny_read: Vec::new(),
            allow_network: true,
        };
        let profile = build_profile(&policy);
        assert!(profile.contains("(allow file-write* (subpath \"/home/user/project\"))"));
        assert!(profile.contains("(allow file-write* (literal \"/dev/tty\"))"));
        assert!(profile.matches("(allow file-write*").count() >= 5);
    }

    #[test]
    fn test_build_profile_from_policy_public() {
        let policy = SandboxPolicy {
            writable_paths: vec![PathBuf::from("/tmp")],
            deny_read: Vec::new(),
            allow_network: true,
        };
        let profile = build_profile_from_policy(&policy);
        assert!(profile.contains("(allow file-write* (subpath \"/tmp\"))"));
    }
}
