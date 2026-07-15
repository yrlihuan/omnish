//! Unified sandbox abstraction layer.
//!
//! Callers describe restrictions via [`SandboxPolicy`], then call
//! [`sandbox_command()`] to get a ready-to-use [`Command`].

pub(crate) mod bwrap;
pub(crate) mod landlock;
#[cfg(target_os = "macos")]
pub(crate) mod seatbelt;

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackendType {
    Bwrap,
    Landlock,
    #[cfg(target_os = "macos")]
    MacosSeatbelt,
}

/// Result of sandbox backend detection, carrying enough information
/// for the caller to construct appropriate user-facing messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDetectResult {
    /// Preferred backend is available and will be used.
    Preferred(SandboxBackendType),
    /// Preferred backend unavailable; fell back to another.
    Fallback {
        preferred: SandboxBackendType,
        actual: SandboxBackendType,
    },
    /// No backend available at all.
    Unavailable {
        preferred: SandboxBackendType,
    },
    /// Sandbox explicitly disabled by client configuration.
    Disabled,
}

impl SandboxDetectResult {
    /// The backend to actually use, if any.
    pub fn backend(&self) -> Option<SandboxBackendType> {
        match self {
            Self::Preferred(b) | Self::Fallback { actual: b, .. } => Some(*b),
            Self::Unavailable { .. } | Self::Disabled => None,
        }
    }
}

impl SandboxBackendType {
    pub fn from_config(s: &str) -> Option<Self> {
        match s {
            "bwrap" => Some(Self::Bwrap),
            "landlock" => Some(Self::Landlock),
            #[cfg(target_os = "macos")]
            "macos" => Some(Self::MacosSeatbelt),
            _ => None,
        }
    }
}

pub struct SandboxPolicy {
    pub writable_paths: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
    pub allow_network: bool,
}

pub use bwrap::BwrapUnavailableReason;

pub fn is_available(backend: SandboxBackendType) -> bool {
    match backend {
        SandboxBackendType::Bwrap => bwrap::is_available(),
        SandboxBackendType::Landlock => landlock::is_available(),
        #[cfg(target_os = "macos")]
        SandboxBackendType::MacosSeatbelt => true,
    }
}

/// Returns the reason bwrap is unavailable, or `None` if it works.
pub fn bwrap_unavailable_reason() -> Option<BwrapUnavailableReason> {
    bwrap::unavailable_reason()
}

/// Detect the best available sandbox backend, starting from `preferred`.
/// Returns a [`SandboxDetectResult`] describing what was found, without
/// printing any messages - the caller decides how to present the outcome.
pub fn detect_backend_status(preferred: SandboxBackendType) -> SandboxDetectResult {
    if is_available(preferred) {
        return SandboxDetectResult::Preferred(preferred);
    }

    let fallback = match preferred {
        SandboxBackendType::Bwrap => Some(SandboxBackendType::Landlock),
        SandboxBackendType::Landlock => Some(SandboxBackendType::Bwrap),
        #[cfg(target_os = "macos")]
        SandboxBackendType::MacosSeatbelt => None,
    };

    if let Some(fb) = fallback {
        if is_available(fb) {
            return SandboxDetectResult::Fallback {
                preferred,
                actual: fb,
            };
        }
    }

    SandboxDetectResult::Unavailable { preferred }
}

/// Convenience wrapper: returns only the resolved backend (if any).
pub fn detect_backend(preferred: SandboxBackendType) -> Option<SandboxBackendType> {
    detect_backend_status(preferred).backend()
}

pub fn sandbox_command(
    backend: SandboxBackendType,
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    match backend {
        SandboxBackendType::Bwrap => bwrap::sandbox_command(policy, executable, args),
        SandboxBackendType::Landlock => landlock::sandbox_command(policy, executable, args),
        #[cfg(target_os = "macos")]
        SandboxBackendType::MacosSeatbelt => seatbelt::sandbox_command(policy, executable, args),
    }
}

/// Apply sandbox restrictions in the current process (Landlock only).
/// Used in pre_exec contexts (e.g. handle_lock) where the sandbox must
/// be applied after fork but before exec.
/// No-op on non-Linux or when Landlock is unavailable.
pub fn apply_in_process(policy: &SandboxPolicy) -> Result<(), String> {
    landlock::apply_landlock_from_policy(policy)
}

fn git_repo_root(dir: &Path) -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

/// Resolve symlinks so bind-mount destinations are always real paths.
///
/// bwrap cannot bind-mount onto a symlink destination: `--bind <link> <link>`
/// fails with "Unable to mount source on destination: No such file or
/// directory" and aborts the whole sandbox, so a symlinked cwd or a symlinked
/// `~/.omnish` breaks every tool call (issue #648). Canonicalizing also lets
/// cwd and git-root dedup when they resolve to the same place. Falls back to
/// the original path when it does not exist or cannot be resolved (the backends
/// already skip non-existent paths).
fn canonicalize_or_keep(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// Canonicalize every path and drop duplicates while preserving order.
fn canonicalize_and_dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .map(canonicalize_or_keep)
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

fn common_writable_paths(cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = vec![
        "/tmp".into(),
        "/dev/null".into(),
        "/dev/ptmx".into(),
        "/dev/pts".into(),
        "/dev/tty".into(),
        "/dev/shm".into(),
    ];

    #[cfg(target_os = "linux")]
    {
        paths.push("/home/linuxbrew/.linuxbrew".into());
        paths.push("/var/spool/cron".into());
    }
    #[cfg(target_os = "macos")]
    {
        paths.push("/opt/homebrew".into());
    }

    if let Some(home) = dirs::home_dir() {
        for name in &[
            ".ssh", ".cargo", ".config", ".local", ".claude", ".omnish", ".cache", ".npm",
            ".rustup", ".gnupg", ".docker", ".kube", ".nvm", ".pyenv",
        ] {
            paths.push(home.join(name));
        }
    }

    if let Some(cwd) = cwd {
        if let Some(root) = git_repo_root(cwd) {
            if root != cwd {
                paths.push(root);
            }
        }
        paths.push(cwd.to_path_buf());
    }

    // Resolve symlinks so bind-mount destinations are real paths (issue #648).
    canonicalize_and_dedup(paths)
}

pub fn plugin_policy(data_dir: &Path, cwd: Option<&Path>) -> SandboxPolicy {
    let mut writable = common_writable_paths(cwd);
    let data_dir = canonicalize_or_keep(data_dir.to_path_buf());
    if !writable.contains(&data_dir) {
        writable.insert(0, data_dir);
    }
    SandboxPolicy {
        writable_paths: writable,
        deny_read: Vec::new(),
        allow_network: true,
    }
}

pub fn lock_policy(cwd: Option<&Path>) -> SandboxPolicy {
    SandboxPolicy {
        writable_paths: common_writable_paths(cwd),
        deny_read: Vec::new(),
        allow_network: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #648: a symlinked cwd must be resolved to its real path so bwrap
    /// never tries to bind-mount onto a symlink destination.
    #[test]
    fn symlinked_cwd_is_canonicalized_in_writable_paths() {
        let tmp = std::env::temp_dir().join(format!("omnish_sbx_test_{}", std::process::id()));
        let real = tmp.join("realdir");
        let link = tmp.join("linkdir");
        std::fs::create_dir_all(&real).unwrap();
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let real_canon = std::fs::canonicalize(&real).unwrap();
        let paths = common_writable_paths(Some(&link));

        assert!(
            paths.contains(&real_canon),
            "resolved real cwd {:?} must be present, got {:?}",
            real_canon,
            paths
        );
        assert!(
            !paths.contains(&link),
            "symlink path {:?} must not be bound directly, got {:?}",
            link,
            paths
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Duplicate resolved paths (e.g. cwd == git root after resolution) collapse.
    #[test]
    fn writable_paths_have_no_duplicates() {
        let paths = common_writable_paths(None);
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), paths.len(), "writable paths must be unique");
    }
}
