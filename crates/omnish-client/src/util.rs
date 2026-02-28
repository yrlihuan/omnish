/// Get the shell's current working directory
#[cfg(target_os = "linux")]
pub fn get_shell_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Get the shell's current working directory using proc_pidpath on macOS
/// Note: proc_pidpath returns the executable path, not CWD.
/// Getting CWD on macOS requires additional work (e.g., using lsof).
/// For now, return None to fall back to other methods.
#[cfg(target_os = "macos")]
pub fn get_shell_cwd(_pid: u32) -> Option<String> {
    // On macOS, getting CWD of another process is complex.
    // We could use lsof -p <pid> and parse, but it's slow.
    // Return None to fall back to session's initial CWD.
    None
}

/// Get the shell's current working directory - fallback for unsupported platforms
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn get_shell_cwd(_pid: u32) -> Option<String> {
    None
}
