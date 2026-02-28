/// Get the shell's current working directory
#[cfg(target_os = "linux")]
pub fn get_shell_cwd(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Get the shell's current working directory on macOS using lsof
#[cfg(target_os = "macos")]
pub fn get_shell_cwd(pid: u32) -> Option<String> {
    use std::process::Command;

    // Use lsof to get the current working directory of a process
    // Format: "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME"
    let output = Command::new("lsof")
        .args(["-p", &pid.to_string(), "-a", "-d", "cwd"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Parse output: find the line with "cwd" in FD column and get the NAME (last field)
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip the header line
        if line.starts_with("COMMAND") || line.starts_with("lsof:") {
            continue;
        }
        // Look for lines with "cwd" (file descriptor column)
        // The FD column is the 4th field
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 && parts[3] == "cwd" {
            // The path is the last field
            if let Some(path) = parts.last() {
                if !path.is_empty() && path.starts_with('/') {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

/// Get the shell's current working directory - fallback for unsupported platforms
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn get_shell_cwd(_pid: u32) -> Option<String> {
    None
}
