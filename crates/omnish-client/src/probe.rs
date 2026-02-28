use std::collections::HashMap;

pub trait Probe: Send + Sync {
    fn key(&self) -> &str;
    fn collect(&self) -> Option<String>;
}

pub struct ProbeSet {
    probes: Vec<Box<dyn Probe>>,
}

impl ProbeSet {
    pub fn new() -> Self {
        Self { probes: Vec::new() }
    }

    pub fn add(&mut self, probe: Box<dyn Probe>) {
        self.probes.push(probe);
    }

    pub fn collect_all(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for probe in &self.probes {
            if let Some(value) = probe.collect() {
                map.insert(probe.key().to_string(), value);
            }
        }
        map
    }
}

pub struct ShellProbe;
impl Probe for ShellProbe {
    fn key(&self) -> &str { "shell" }
    fn collect(&self) -> Option<String> { std::env::var("SHELL").ok() }
}

pub struct PidProbe(pub u32);
impl Probe for PidProbe {
    fn key(&self) -> &str { "pid" }
    fn collect(&self) -> Option<String> { Some(self.0.to_string()) }
}

pub struct TtyProbe;
impl Probe for TtyProbe {
    fn key(&self) -> &str { "tty" }
    fn collect(&self) -> Option<String> { std::env::var("TTY").ok() }
}

pub struct CwdProbe;
impl Probe for CwdProbe {
    fn key(&self) -> &str { "cwd" }
    fn collect(&self) -> Option<String> {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    }
}

pub struct HostnameProbe;
impl Probe for HostnameProbe {
    fn key(&self) -> &str { "hostname" }
    fn collect(&self) -> Option<String> {
        nix::unistd::gethostname()
            .ok()
            .and_then(|h| h.into_string().ok())
    }
}

pub struct ShellCwdProbe(pub u32);
impl Probe for ShellCwdProbe {
    fn key(&self) -> &str { "shell_cwd" }
    fn collect(&self) -> Option<String> {
        super::util::get_shell_cwd(self.0)
    }
}

#[allow(dead_code)]
pub struct ChildProcessProbe(pub u32);

#[cfg(target_os = "linux")]
impl Probe for ChildProcessProbe {
    fn key(&self) -> &str { "child_process" }
    fn collect(&self) -> Option<String> {
        let children_path = format!("/proc/{}/task/{}/children", self.0, self.0);
        let children_str = std::fs::read_to_string(&children_path).unwrap_or_default();
        let child_pid: Option<i32> = children_str
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .last();
        match child_pid {
            Some(pid) => {
                let name = std::fs::read_to_string(format!("/proc/{}/comm", pid))
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                Some(format!("{}:{}", name, pid))
            }
            None => Some(String::new()),
        }
    }
}

#[cfg(target_os = "macos")]
impl Probe for ChildProcessProbe {
    fn key(&self) -> &str { "child_process" }
    fn collect(&self) -> Option<String> {
        use std::process::Command;

        // On macOS, use ps to get child processes
        // Get all processes with the shell as parent
        let output = Command::new("ps")
            .args(["-o", "pid=", "-o", "comm=", "-ax"])
            .output()
            .ok()?;

        if !output.status.success() {
            return Some(String::new());
        }

        let shell_pid = self.0 as i32;

        // Find child processes of our shell
        // We need to find processes whose PPID equals our shell's PID
        // Use ps -ax -o pid= -o ppid= -o comm=
        let output2 = Command::new("ps")
            .args(["-ax", "-o", "pid=", "-o", "ppid=", "-o", "comm="])
            .output()
            .ok()?;

        if !output2.status.success() {
            return Some(String::new());
        }

        let stdout2 = String::from_utf8_lossy(&output2.stdout);
        let mut child_info: Option<(i32, String)> = None;

        for line in stdout2.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }

            let pid: i32 = parts[0].parse().ok()?;
            let ppid: i32 = parts[1].parse().ok()?;
            let comm = parts[2];

            if ppid == shell_pid {
                // Skip the shell process itself
                if pid == shell_pid {
                    continue;
                }
                // Get the most recently started child (last one wins)
                child_info = Some((pid, comm.to_string()));
            }
        }

        match child_info {
            Some((pid, name)) => Some(format!("{}:{}", name, pid)),
            None => Some(String::new()),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Probe for ChildProcessProbe {
    fn key(&self) -> &str { "child_process" }
    fn collect(&self) -> Option<String> {
        Some(String::new())
    }
}

pub fn default_session_probes(child_pid: u32) -> ProbeSet {
    let mut set = ProbeSet::new();
    set.add(Box::new(ShellProbe));
    set.add(Box::new(PidProbe(child_pid)));
    set.add(Box::new(TtyProbe));
    set.add(Box::new(CwdProbe));
    set.add(Box::new(HostnameProbe));
    set
}

pub fn default_polling_probes(child_pid: u32) -> ProbeSet {
    let mut set = ProbeSet::new();
    set.add(Box::new(HostnameProbe));
    set.add(Box::new(ShellCwdProbe(child_pid)));
    set.add(Box::new(ChildProcessProbe(child_pid)));
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysProbe;
    impl Probe for AlwaysProbe {
        fn key(&self) -> &str { "always" }
        fn collect(&self) -> Option<String> { Some("yes".to_string()) }
    }

    struct NeverProbe;
    impl Probe for NeverProbe {
        fn key(&self) -> &str { "never" }
        fn collect(&self) -> Option<String> { None }
    }

    #[test]
    fn test_collect_all_skips_none() {
        let mut set = ProbeSet::new();
        set.add(Box::new(AlwaysProbe));
        set.add(Box::new(NeverProbe));
        let attrs = set.collect_all();
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs.get("always").unwrap(), "yes");
        assert!(!attrs.contains_key("never"));
    }

    #[test]
    fn test_pid_probe() {
        let probe = PidProbe(42);
        assert_eq!(probe.key(), "pid");
        assert_eq!(probe.collect(), Some("42".to_string()));
    }

    #[test]
    fn test_cwd_probe() {
        let probe = CwdProbe;
        assert_eq!(probe.key(), "cwd");
        // cwd should always succeed in test env
        assert!(probe.collect().is_some());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_shell_cwd_probe_returns_path_for_self() {
        let pid = std::process::id();
        let probe = ShellCwdProbe(pid);
        assert_eq!(probe.key(), "shell_cwd");
        let cwd = probe.collect();
        assert!(cwd.is_some(), "should read own cwd from /proc");
        let expected = std::env::current_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(cwd.unwrap(), expected);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_shell_cwd_probe_returns_path_for_self_on_macos() {
        let pid = std::process::id();
        let probe = ShellCwdProbe(pid);
        assert_eq!(probe.key(), "shell_cwd");
        // On macOS, ShellCwdProbe uses lsof to get the path
        let cwd = probe.collect();
        assert!(cwd.is_some(), "should return CWD on macOS via lsof");
        let expected = std::env::current_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(cwd.unwrap(), expected);
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn test_shell_cwd_probe_returns_none_for_self() {
        // On other platforms (not linux/macos), ShellCwdProbe returns None
        let pid = std::process::id();
        let probe = ShellCwdProbe(pid);
        assert_eq!(probe.key(), "shell_cwd");
        let cwd = probe.collect();
        assert!(cwd.is_none(), "should return None on unsupported platforms");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_shell_cwd_probe_returns_none_for_bad_pid() {
        let probe = ShellCwdProbe(999999999);
        assert_eq!(probe.collect(), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_shell_cwd_probe_returns_none_for_bad_pid_on_macos() {
        // On macOS, lsof returns error for bad PID
        let probe = ShellCwdProbe(999999999);
        assert_eq!(probe.collect(), None);
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn test_shell_cwd_probe_returns_none_for_bad_pid() {
        // On other platforms, always returns None
        let probe = ShellCwdProbe(999999999);
        assert_eq!(probe.collect(), None);
    }

    #[test]
    fn test_child_process_probe_key() {
        let probe = ChildProcessProbe(std::process::id());
        assert_eq!(probe.key(), "child_process");
    }

    #[test]
    fn test_child_process_probe_returns_string_or_empty() {
        let probe = ChildProcessProbe(std::process::id());
        let result = probe.collect();
        assert!(result.is_some());
    }
}
