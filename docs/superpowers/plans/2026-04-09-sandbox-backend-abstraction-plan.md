# Sandbox Backend Abstraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Abstract sandbox implementations behind a unified `SandboxPolicy` + `sandbox_command()` API, add bwrap as a new backend alongside existing Landlock and macOS seatbelt.

**Architecture:** Create `omnish-plugin/src/sandbox/` module with `mod.rs` (public types + dispatch), `landlock.rs`, `seatbelt.rs`, `bwrap.rs`. Callers build a `SandboxPolicy` describing restrictions, then call `sandbox_command()` which returns a ready-to-use `Command`. Config selects backend with automatic fallback.

**Tech Stack:** Rust, bubblewrap (bwrap), Landlock, macOS sandbox-exec

**Spec:** `docs/plans/2026-04-09-sandbox-backend-abstraction-design.md`

---

### Task 1: Add `backend` field to `SandboxConfig`

**Files:**
- Modify: `crates/omnish-common/src/config.rs:280-294`

- [ ] **Step 1: Add backend field to SandboxConfig**

In `crates/omnish-common/src/config.rs`, replace the existing `SandboxConfig`:

```rust
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxConfig {
    /// Per-tool permit rules. Key is tool_name (e.g. "bash").
    /// When any rule matches, the tool runs without Landlock sandbox.
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}
```

with:

```rust
fn default_sandbox_backend() -> String {
    if cfg!(target_os = "macos") {
        "macos".to_string()
    } else {
        "bwrap".to_string()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SandboxConfig {
    /// Sandbox backend: "bwrap" | "landlock" | "macos"
    /// Default: "bwrap" on Linux, "macos" on macOS
    #[serde(default = "default_sandbox_backend")]
    pub backend: String,
    /// Per-tool permit rules. Key is tool_name (e.g. "bash").
    /// When any rule matches, the tool runs without sandbox.
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: default_sandbox_backend(),
            plugins: HashMap::new(),
        }
    }
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo build --release 2>&1 | tail -5`
Expected: successful build, no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-common/src/config.rs
git commit -m "feat(sandbox): add backend field to SandboxConfig"
```

---

### Task 2: Create `sandbox/mod.rs` with public types and policy builders

**Files:**
- Create: `crates/omnish-plugin/src/sandbox/mod.rs`

- [ ] **Step 1: Create the sandbox module directory**

```bash
mkdir -p crates/omnish-plugin/src/sandbox
```

- [ ] **Step 2: Write `sandbox/mod.rs`**

Create `crates/omnish-plugin/src/sandbox/mod.rs`:

```rust
//! Unified sandbox abstraction layer.
//!
//! Callers describe restrictions via [`SandboxPolicy`], then call
//! [`sandbox_command()`] to get a ready-to-use [`Command`].

mod bwrap;
mod landlock;
#[cfg(target_os = "macos")]
mod seatbelt;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Available sandbox backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackendType {
    Bwrap,
    Landlock,
    #[cfg(target_os = "macos")]
    MacosSeatbelt,
}

impl SandboxBackendType {
    /// Parse from config string. Returns `None` for unknown values.
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

/// Describes what restrictions the sandbox should enforce.
pub struct SandboxPolicy {
    /// Paths that should be writable. Everything else is read-only.
    pub writable_paths: Vec<PathBuf>,
    /// Paths that should be denied for reading.
    pub deny_read: Vec<PathBuf>,
    /// Whether network access is allowed (reserved for future use, currently always true).
    pub allow_network: bool,
}

/// Check whether a specific backend is available on this system.
pub fn is_available(backend: SandboxBackendType) -> bool {
    match backend {
        SandboxBackendType::Bwrap => bwrap::is_available(),
        SandboxBackendType::Landlock => landlock::is_available(),
        #[cfg(target_os = "macos")]
        SandboxBackendType::MacosSeatbelt => true,
    }
}

/// Select a backend: try `preferred`, fall back to alternatives, return `None` if nothing works.
///
/// Fallback chain:
/// - Linux: bwrap → landlock → None
/// - macOS: MacosSeatbelt → None
pub fn detect_backend(preferred: SandboxBackendType) -> Option<SandboxBackendType> {
    if is_available(preferred) {
        return Some(preferred);
    }
    eprintln!(
        "[sandbox] preferred backend {:?} not available, trying fallback",
        preferred,
    );

    let fallback = match preferred {
        SandboxBackendType::Bwrap => Some(SandboxBackendType::Landlock),
        SandboxBackendType::Landlock => Some(SandboxBackendType::Bwrap),
        #[cfg(target_os = "macos")]
        SandboxBackendType::MacosSeatbelt => None,
    };

    if let Some(fb) = fallback {
        if is_available(fb) {
            eprintln!("[sandbox] using fallback backend {:?}", fb);
            return Some(fb);
        }
    }

    eprintln!("[sandbox] no sandbox backend available");
    None
}

/// Build a sandboxed [`Command`].
///
/// The returned `Command` has sandbox restrictions applied according to `policy`.
/// The caller should still set stdin/stdout/stderr as needed.
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

/// Detect the git repository root for a given directory.
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

/// Common writable paths shared by all policy builders.
fn common_writable_paths(cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = vec![
        "/tmp".into(),
        "/dev/null".into(),
        "/dev/ptmx".into(),
        "/dev/pts".into(),
        "/dev/tty".into(),
        "/dev/shm".into(),
    ];

    // Platform-specific paths
    #[cfg(target_os = "linux")]
    {
        paths.push("/home/linuxbrew/.linuxbrew".into());
        paths.push("/var/spool/cron".into());
    }
    #[cfg(target_os = "macos")]
    {
        paths.push("/opt/homebrew".into());
    }

    // Well-known dotdirs
    if let Some(home) = dirs::home_dir() {
        for name in &[
            ".ssh", ".cargo", ".config", ".local", ".claude", ".omnish",
            ".cache", ".npm", ".rustup", ".gnupg", ".docker", ".kube",
            ".nvm", ".pyenv",
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

    paths
}

/// Build a [`SandboxPolicy`] for plugin tool execution.
///
/// Writable: `data_dir` + common paths + cwd + git repo root.
pub fn plugin_policy(data_dir: &Path, cwd: Option<&Path>) -> SandboxPolicy {
    let mut writable = common_writable_paths(cwd);
    writable.insert(0, data_dir.to_path_buf());
    SandboxPolicy {
        writable_paths: writable,
        deny_read: Vec::new(),
        allow_network: true,
    }
}

/// Build a [`SandboxPolicy`] for shell lock mode (`/test lock on`).
///
/// Writable: common paths + cwd + git repo root (no `data_dir`).
pub fn lock_policy(cwd: Option<&Path>) -> SandboxPolicy {
    SandboxPolicy {
        writable_paths: common_writable_paths(cwd),
        deny_read: Vec::new(),
        allow_network: true,
    }
}
```

- [ ] **Step 3: Build (will fail - backend modules not yet created)**

Run: `cargo build --release 2>&1 | tail -5`
Expected: compilation errors about missing `bwrap`, `landlock`, `seatbelt` modules. This confirms the module structure is wired up.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-plugin/src/sandbox/
git commit -m "feat(sandbox): add mod.rs with SandboxPolicy and public API (backends pending)"
```

---

### Task 3: Create `sandbox/landlock.rs` - migrate existing Landlock code

**Files:**
- Create: `crates/omnish-plugin/src/sandbox/landlock.rs`

- [ ] **Step 1: Write `sandbox/landlock.rs`**

Create `crates/omnish-plugin/src/sandbox/landlock.rs`. This migrates the existing Landlock code from `lib.rs` into the new sandbox interface:

```rust
//! Landlock sandbox backend (Linux only).
//!
//! Applies filesystem restrictions via the Landlock LSM using `pre_exec`.
//! Requires kernel >= 5.13.

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "linux")]
use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, ABI,
};

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

/// Check whether Landlock is available on this system (kernel >= 5.13).
pub fn is_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        detect_abi().is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Build a sandboxed Command using Landlock via `pre_exec`.
pub fn sandbox_command(
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    let mut cmd = Command::new(executable);
    cmd.args(args);

    #[cfg(target_os = "linux")]
    {
        let writable: Vec<std::path::PathBuf> = policy.writable_paths.clone();
        unsafe {
            cmd.pre_exec(move || {
                apply_landlock(&writable.iter().map(|p| p.as_path()).collect::<Vec<_>>())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))
            });
        }
    }

    Ok(cmd)
}

#[cfg(target_os = "linux")]
fn detect_abi() -> Option<ABI> {
    let mut utsname: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut utsname) } != 0 {
        return None;
    }
    let release = unsafe { std::ffi::CStr::from_ptr(utsname.release.as_ptr()) };
    let release = release.to_str().ok()?;
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;

    let ver = (major, minor);
    if ver >= (6, 10) {
        Some(ABI::V5)
    } else if ver >= (6, 7) {
        Some(ABI::V4)
    } else if ver >= (6, 2) {
        Some(ABI::V3)
    } else if ver >= (5, 19) {
        Some(ABI::V2)
    } else if ver >= (5, 13) {
        Some(ABI::V1)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn apply_landlock(writable_paths: &[&Path]) -> Result<(), String> {
    let abi = match detect_abi() {
        Some(abi) => abi,
        None => return Ok(()),
    };
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
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced | RulesetStatus::NotEnforced => Ok(()),
    }
}
```

- [ ] **Step 2: Build (will still fail - bwrap and seatbelt modules missing)**

Run: `cargo build --release 2>&1 | tail -5`
Expected: errors about missing `bwrap` and/or `seatbelt` modules, but no errors from `landlock.rs`.

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-plugin/src/sandbox/landlock.rs
git commit -m "feat(sandbox): add landlock backend module (migrated from lib.rs)"
```

---

### Task 4: Create `sandbox/seatbelt.rs` - migrate existing macOS code

**Files:**
- Create: `crates/omnish-plugin/src/sandbox/seatbelt.rs`

- [ ] **Step 1: Write `sandbox/seatbelt.rs`**

Create `crates/omnish-plugin/src/sandbox/seatbelt.rs`. Migrates existing macOS sandbox-exec code:

```rust
//! macOS seatbelt sandbox backend.
//!
//! Wraps commands with `sandbox-exec -p <profile>` using Apple's seatbelt framework.

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// macOS seatbelt is always available on macOS.
#[cfg(target_os = "macos")]
pub fn is_available() -> bool {
    true
}

/// Build a sandboxed Command using `sandbox-exec`.
#[cfg(target_os = "macos")]
pub fn sandbox_command(
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    let profile = build_profile(policy);
    let mut cmd = Command::new("sandbox-exec");
    cmd.args(["-p", &profile, &executable.to_string_lossy()]);
    cmd.args(args);
    Ok(cmd)
}

/// Escape a path for use inside a `.sb` profile.
#[cfg(any(target_os = "macos", test))]
fn escape_sb_path(path: &str) -> String {
    let cleaned: String = path.chars().filter(|c| !c.is_control()).collect();
    cleaned.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build a `.sb` profile string from a SandboxPolicy.
#[cfg(any(target_os = "macos", test))]
fn build_profile(policy: &SandboxPolicy) -> String {
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (allow sysctl-read)\n\
         (deny file-write* (subpath \"/\"))\n",
    );

    for path in &policy.writable_paths {
        let escaped = escape_sb_path(&path.to_string_lossy());
        // Use literal for files, subpath for directories
        if path.is_file() {
            profile.push_str(&format!("(allow file-write* (literal \"{escaped}\"))\n"));
        } else {
            profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
        }
    }

    profile
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
    fn test_escape_sb_path_backslash_before_quote() {
        assert_eq!(escape_sb_path("a\\\"b"), "a\\\\\\\"b");
    }

    #[test]
    fn test_build_profile_basic() {
        let policy = SandboxPolicy {
            writable_paths: vec![PathBuf::from("/tmp"), PathBuf::from("/data/plugin")],
            deny_read: Vec::new(),
            allow_network: true,
        };
        let profile = build_profile(&policy);
        assert!(profile.contains("(allow default)"));
        assert!(profile.contains("(deny file-write* (subpath \"/\"))"));
        assert!(profile.contains("/tmp"));
        assert!(profile.contains("/data/plugin"));
    }
}
```

- [ ] **Step 2: Build (will still fail - bwrap module missing on Linux)**

Run: `cargo build --release 2>&1 | tail -5`
Expected: error about missing `bwrap` module only. `seatbelt.rs` compiles (or is skipped on Linux via `cfg`).

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-plugin/src/sandbox/seatbelt.rs
git commit -m "feat(sandbox): add macOS seatbelt backend module (migrated from lib.rs)"
```

---

### Task 5: Create `sandbox/bwrap.rs` - new bubblewrap backend

**Files:**
- Create: `crates/omnish-plugin/src/sandbox/bwrap.rs`
- Modify: `crates/omnish-plugin/Cargo.toml`

- [ ] **Step 1: Add `which` dependency to Cargo.toml**

In `crates/omnish-plugin/Cargo.toml`, add under `[dependencies]`:

```toml
which = "7"
```

- [ ] **Step 2: Write `sandbox/bwrap.rs`**

Create `crates/omnish-plugin/src/sandbox/bwrap.rs`:

```rust
//! Bubblewrap (bwrap) sandbox backend (Linux only).
//!
//! Wraps commands with `bwrap` for filesystem isolation using bind mounts.
//! Requires the `bwrap` binary to be installed.

use super::SandboxPolicy;
use std::path::Path;
use std::process::Command;

/// Check whether bwrap is installed and accessible.
pub fn is_available() -> bool {
    which::which("bwrap").is_ok()
}

/// Build a sandboxed Command using bwrap.
pub fn sandbox_command(
    policy: &SandboxPolicy,
    executable: &Path,
    args: &[&str],
) -> Result<Command, String> {
    let mut cmd = Command::new("bwrap");

    // Session and lifecycle
    cmd.args(["--new-session", "--die-with-parent"]);

    // Read-only root filesystem
    cmd.args(["--ro-bind", "/", "/"]);

    // Device filesystem
    cmd.args(["--dev", "/dev"]);

    // Writable paths: bind-mount each path read-write over the read-only root
    for path in &policy.writable_paths {
        let path_str = path.to_string_lossy();
        // Skip /dev/* paths - --dev /dev already handles them
        if path_str.starts_with("/dev/") || path_str == "/dev" {
            continue;
        }
        if path.exists() {
            cmd.args(["--bind", &path_str, &path_str]);
        }
    }

    // Deny read: hide paths by mounting tmpfs (directories) or /dev/null (files)
    for path in &policy.deny_read {
        if !path.exists() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if path.is_dir() {
            cmd.args(["--tmpfs", &path_str]);
        } else {
            cmd.args(["--ro-bind", "/dev/null", &path_str]);
        }
    }

    // The actual command to execute
    cmd.arg("--");
    cmd.arg(executable);
    cmd.args(args);

    Ok(cmd)
}
```

- [ ] **Step 3: Build**

Run: `cargo build --release 2>&1 | tail -5`
Expected: successful build, all modules compile.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-plugin/Cargo.toml crates/omnish-plugin/src/sandbox/bwrap.rs
git commit -m "feat(sandbox): add bubblewrap (bwrap) backend"
```

---

### Task 6: Wire up `sandbox` module in `lib.rs` and preserve backward compat

**Files:**
- Modify: `crates/omnish-plugin/src/lib.rs`

- [ ] **Step 1: Replace sandbox code in `lib.rs` with module re-export**

Replace the entire content of `crates/omnish-plugin/src/lib.rs` with:

```rust
//! Shared plugin infrastructure: sandbox backends and tool implementations.

pub mod formatter;
pub mod sandbox;
pub mod tools;

// Re-export sandbox public API for convenience
pub use sandbox::{
    detect_backend, is_available, lock_policy, plugin_policy, sandbox_command, SandboxBackendType,
    SandboxPolicy,
};

// --- Backward compatibility shims ---
// These delegate to the new sandbox module so existing callers keep working
// until they are migrated in Tasks 7-8.

/// Check whether Landlock is supported (backward compat).
pub fn is_landlock_supported() -> bool {
    is_available(SandboxBackendType::Landlock)
}

/// Apply Landlock sandbox for plugins (backward compat).
#[cfg(target_os = "linux")]
pub fn apply_sandbox(
    data_dir: &std::path::Path,
    cwd: Option<&std::path::Path>,
) -> Result<(), String> {
    // This is only used via pre_exec, so we apply Landlock directly
    let policy = plugin_policy(data_dir, cwd);
    sandbox::landlock::apply_landlock_from_policy(&policy)
}

/// Apply Landlock sandbox for shell lock (backward compat).
#[cfg(target_os = "linux")]
pub fn apply_lock_sandbox(cwd: Option<&std::path::Path>) -> Result<(), String> {
    let policy = lock_policy(cwd);
    sandbox::landlock::apply_landlock_from_policy(&policy)
}

/// macOS sandbox profile (backward compat).
#[cfg(target_os = "macos")]
pub fn sandbox_profile(
    data_dir: &std::path::Path,
    cwd: Option<&std::path::Path>,
) -> String {
    let policy = plugin_policy(data_dir, cwd);
    sandbox::seatbelt::build_profile_from_policy(&policy)
}

// No-ops for non-Linux
#[cfg(not(target_os = "linux"))]
pub fn apply_sandbox(
    _data_dir: &std::path::Path,
    _cwd: Option<&std::path::Path>,
) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn apply_lock_sandbox(_cwd: Option<&std::path::Path>) -> Result<(), String> {
    Ok(())
}
```

- [ ] **Step 2: Add backward-compat helper to `landlock.rs`**

Add to the end of `crates/omnish-plugin/src/sandbox/landlock.rs`:

```rust
/// Apply Landlock directly from a SandboxPolicy (used by backward-compat shims in lib.rs).
/// This runs in the current process (via pre_exec), not by wrapping a Command.
#[cfg(target_os = "linux")]
pub fn apply_landlock_from_policy(policy: &SandboxPolicy) -> Result<(), String> {
    let refs: Vec<&Path> = policy.writable_paths.iter().map(|p| p.as_path()).collect();
    apply_landlock(&refs)
}
```

Make the function `pub(crate)` visible by changing the module visibility. In `sandbox/mod.rs`, change:

```rust
mod landlock;
```

to:

```rust
pub(crate) mod landlock;
```

- [ ] **Step 3: Add backward-compat helper to `seatbelt.rs`**

Add to the end of `crates/omnish-plugin/src/sandbox/seatbelt.rs`:

```rust
/// Build a seatbelt profile from a SandboxPolicy (used by backward-compat shim in lib.rs).
#[cfg(target_os = "macos")]
pub fn build_profile_from_policy(policy: &SandboxPolicy) -> String {
    build_profile(policy)
}
```

Similarly in `sandbox/mod.rs`, change:

```rust
#[cfg(target_os = "macos")]
mod seatbelt;
```

to:

```rust
#[cfg(target_os = "macos")]
pub(crate) mod seatbelt;
```

- [ ] **Step 4: Build and run tests**

Run: `cargo build --release 2>&1 | tail -5`
Expected: successful build.

Run: `cargo test -p omnish-plugin 2>&1 | tail -10`
Expected: existing tests pass. (macOS seatbelt profile tests now live in `seatbelt.rs`, old tests in `lib.rs` can be removed.)

- [ ] **Step 5: Remove old tests from `lib.rs`**

Remove the `#[cfg(test)] mod tests { ... }` block from `lib.rs` - those tests are now covered by `seatbelt.rs::tests`.

- [ ] **Step 6: Build and test again**

Run: `cargo build --release && cargo test -p omnish-plugin 2>&1 | tail -10`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-plugin/src/
git commit -m "refactor(sandbox): wire up sandbox module, add backward-compat shims"
```

---

### Task 7: Migrate `client_plugin.rs` to use `sandbox_command()`

**Files:**
- Modify: `crates/omnish-client/src/client_plugin.rs`

- [ ] **Step 1: Rewrite `execute_tool` to use sandbox_command**

Replace the content of `crates/omnish-client/src/client_plugin.rs`:

```rust
//! Client-side tool execution via short-lived plugin processes.
//! Spawns a fresh process per tool call: writes JSON to stdin, reads JSON from stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Executes client-side tools by spawning short-lived plugin processes.
pub struct ClientPluginManager {
    plugin_bin: std::path::PathBuf,
}

/// Result of executing a plugin tool.
pub struct PluginOutput {
    pub content: String,
    pub is_error: bool,
    pub needs_summarization: bool,
}

#[derive(serde::Deserialize)]
struct PluginResponse {
    content: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    needs_summarization: bool,
}

impl ClientPluginManager {
    pub fn new() -> Self {
        let plugin_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("omnish-plugin")))
            .unwrap_or_else(|| std::path::PathBuf::from("omnish-plugin"));
        Self { plugin_bin }
    }

    /// Execute a tool via a short-lived plugin process.
    ///
    /// - `plugin_name`: "builtin" or external plugin directory name
    /// - `tool_name`: the specific tool within the plugin
    /// - `input`: tool input JSON
    /// - `cwd`: optional working directory to inject into input
    /// - `sandboxed`: whether to apply platform sandbox
    /// - `sandbox_backend`: which sandbox backend to use
    pub fn execute_tool(
        &self,
        plugin_name: &str,
        tool_name: &str,
        input: &serde_json::Value,
        cwd: Option<&str>,
        sandboxed: bool,
        sandbox_backend: Option<omnish_plugin::SandboxBackendType>,
    ) -> PluginOutput {
        let executable = if plugin_name == "builtin" {
            self.plugin_bin.clone()
        } else {
            omnish_common::config::omnish_dir()
                .join("plugins")
                .join(plugin_name)
                .join(plugin_name)
        };

        // Inject cwd into input if available
        let effective_input = if let Some(cwd) = cwd {
            let mut patched = input.clone();
            if let Some(obj) = patched.as_object_mut() {
                obj.insert("cwd".to_string(), serde_json::Value::String(cwd.to_string()));
            }
            patched
        } else {
            input.clone()
        };

        let request = serde_json::json!({
            "name": tool_name,
            "input": effective_input,
        });

        let data_dir = omnish_common::config::omnish_dir()
            .join("data")
            .join(plugin_name);
        let _ = std::fs::create_dir_all(&data_dir);

        let cwd_path: Option<std::path::PathBuf> = cwd.map(std::path::PathBuf::from);

        let mut cmd = if sandboxed {
            if let Some(backend) = sandbox_backend.and_then(omnish_plugin::detect_backend) {
                let policy = omnish_plugin::plugin_policy(&data_dir, cwd_path.as_deref());
                match omnish_plugin::sandbox_command(backend, &policy, &executable, &[]) {
                    Ok(c) => c,
                    Err(e) => {
                        return PluginOutput {
                            content: format!("Sandbox setup failed: {}", e),
                            is_error: true,
                            needs_summarization: false,
                        };
                    }
                }
            } else {
                // No sandbox available - run without
                Command::new(&executable)
            }
        } else {
            Command::new(&executable)
        };

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return PluginOutput {
                content: format!("Failed to spawn plugin '{}': {}", plugin_name, e),
                is_error: true,
                needs_summarization: false,
            },
        };

        // Write request to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{}", serde_json::to_string(&request).unwrap());
        }

        // Read response from stdout
        let result = if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => PluginOutput { content: "Plugin produced no output".to_string(), is_error: true, needs_summarization: false },
                Ok(_) => match serde_json::from_str::<PluginResponse>(&line) {
                    Ok(resp) => PluginOutput { content: resp.content, is_error: resp.is_error, needs_summarization: resp.needs_summarization },
                    Err(e) => PluginOutput { content: format!("Invalid plugin response: {e}"), is_error: true, needs_summarization: false },
                },
                Err(e) => PluginOutput { content: format!("Failed to read plugin output: {e}"), is_error: true, needs_summarization: false },
            }
        } else {
            PluginOutput { content: "No stdout from plugin".to_string(), is_error: true, needs_summarization: false }
        };

        let _ = child.wait();
        result
    }
}
```

- [ ] **Step 2: Update all `execute_tool` call sites to pass `sandbox_backend`**

Search for all call sites of `execute_tool` and add the `sandbox_backend` parameter. The backend value should come from the client's resolved sandbox config. Find all call sites:

Run: `grep -rn 'execute_tool(' crates/omnish-client/src/ | grep -v client_plugin.rs`

For each call site, add `sandbox_backend` as the last argument. The backend should be resolved once at startup from `ClientConfig` or daemon config, and stored in a field accessible to the call sites (e.g. a field on the struct that holds the `ClientPluginManager`, or passed through).

The simplest approach: add a `sandbox_backend` field to `ClientPluginManager`:

In `client_plugin.rs`, modify `ClientPluginManager`:

```rust
pub struct ClientPluginManager {
    plugin_bin: std::path::PathBuf,
    sandbox_backend: Option<omnish_plugin::SandboxBackendType>,
}
```

Update `new()` to accept and store the backend:

```rust
pub fn new(sandbox_backend: Option<omnish_plugin::SandboxBackendType>) -> Self {
    let plugin_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("omnish-plugin")))
        .unwrap_or_else(|| std::path::PathBuf::from("omnish-plugin"));
    Self { plugin_bin, sandbox_backend }
}
```

Then simplify `execute_tool` to remove the `sandbox_backend` parameter and use `self.sandbox_backend` instead:

```rust
pub fn execute_tool(
    &self,
    plugin_name: &str,
    tool_name: &str,
    input: &serde_json::Value,
    cwd: Option<&str>,
    sandboxed: bool,
) -> PluginOutput {
    // ... use self.sandbox_backend instead of parameter ...
```

Update the `ClientPluginManager::new()` call site in `main.rs` or wherever it's constructed, to pass the resolved backend:

```rust
let sandbox_backend = omnish_plugin::SandboxBackendType::from_config(&config.sandbox.backend);
let plugin_mgr = ClientPluginManager::new(sandbox_backend);
```

- [ ] **Step 3: Build**

Run: `cargo build --release 2>&1 | tail -10`
Expected: successful build. Fix any remaining call site mismatches.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/
git commit -m "refactor(sandbox): migrate client_plugin to sandbox_command API"
```

---

### Task 8: Migrate `handle_lock` to use sandbox API

**Files:**
- Modify: `crates/omnish-client/src/main.rs:1845-1896`

- [ ] **Step 1: Rewrite `handle_lock` to use sandbox API**

Replace the `handle_lock` function in `crates/omnish-client/src/main.rs`:

```rust
fn handle_lock(
    proxy: &mut PtyProxy,
    master_fd: &mut i32,
    locked: &mut bool,
    lock: bool,
    shell: &str,
    shell_args: &[&str],
    session_id: &str,
    sandbox_backend: Option<omnish_plugin::SandboxBackendType>,
) {
    if lock == *locked {
        let status = if lock { "already locked" } else { "already unlocked" };
        let msg = format!("\r\n{}{}{}\r\n", display::YELLOW, status, display::RESET);
        nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
        return;
    }

    let cwd = get_shell_cwd(proxy.child_pid() as u32)
        .map(std::path::PathBuf::from);

    let mut env = std::collections::HashMap::new();
    env.insert("OMNISH_SESSION_ID".to_string(), session_id.to_string());
    env.insert("SHELL".to_string(), shell.to_string());

    let pre_exec: Option<Box<dyn FnOnce() -> Result<(), String> + Send>> = if lock {
        let backend = sandbox_backend.and_then(omnish_plugin::detect_backend);
        match backend {
            Some(omnish_plugin::SandboxBackendType::Landlock) => {
                let cwd_clone = cwd.clone();
                Some(Box::new(move || {
                    let policy = omnish_plugin::lock_policy(cwd_clone.as_deref());
                    omnish_plugin::sandbox::landlock::apply_landlock_from_policy(&policy)
                }))
            }
            Some(omnish_plugin::SandboxBackendType::Bwrap) => {
                // bwrap wraps the command externally, handled below via respawn_cmd
                None
            }
            #[cfg(target_os = "macos")]
            Some(omnish_plugin::SandboxBackendType::MacosSeatbelt) => {
                // macOS sandbox wraps externally, handled below via respawn_cmd
                None
            }
            None => {
                let msg = format!(
                    "\r\n{}No sandbox backend available{}\r\n",
                    display::YELLOW, display::RESET,
                );
                nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
                return;
            }
        }
    } else {
        None
    };

    // For bwrap/seatbelt lock mode, we need to wrap the shell command
    let (effective_shell, effective_args): (String, Vec<String>) = if lock {
        let backend = sandbox_backend.and_then(omnish_plugin::detect_backend);
        match backend {
            Some(omnish_plugin::SandboxBackendType::Bwrap) => {
                let policy = omnish_plugin::lock_policy(cwd.as_deref());
                let shell_path = std::path::Path::new(shell);
                let args_strs: Vec<&str> = shell_args.iter().copied().collect();
                match omnish_plugin::sandbox_command(
                    omnish_plugin::SandboxBackendType::Bwrap,
                    &policy,
                    shell_path,
                    &args_strs,
                ) {
                    Ok(cmd) => {
                        let program = cmd.get_program().to_string_lossy().to_string();
                        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
                        (program, args)
                    }
                    Err(e) => {
                        let msg = format!(
                            "\r\n{}Sandbox setup failed: {}{}\r\n",
                            display::RED, e, display::RESET,
                        );
                        nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
                        return;
                    }
                }
            }
            #[cfg(target_os = "macos")]
            Some(omnish_plugin::SandboxBackendType::MacosSeatbelt) => {
                let policy = omnish_plugin::lock_policy(cwd.as_deref());
                let shell_path = std::path::Path::new(shell);
                let args_strs: Vec<&str> = shell_args.iter().copied().collect();
                match omnish_plugin::sandbox_command(
                    omnish_plugin::SandboxBackendType::MacosSeatbelt,
                    &policy,
                    shell_path,
                    &args_strs,
                ) {
                    Ok(cmd) => {
                        let program = cmd.get_program().to_string_lossy().to_string();
                        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
                        (program, args)
                    }
                    Err(e) => {
                        let msg = format!(
                            "\r\n{}Sandbox setup failed: {}{}\r\n",
                            display::RED, e, display::RESET,
                        );
                        nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
                        return;
                    }
                }
            }
            _ => (shell.to_string(), shell_args.iter().map(|s| s.to_string()).collect()),
        }
    } else {
        (shell.to_string(), shell_args.iter().map(|s| s.to_string()).collect())
    };

    let effective_args_ref: Vec<&str> = effective_args.iter().map(|s| s.as_str()).collect();

    match proxy.respawn(&effective_shell, &effective_args_ref, env, cwd.as_deref(), pre_exec) {
        Ok(new_fd) => {
            *master_fd = new_fd;
            *locked = lock;
            setup_sigwinch(new_fd);
            if let Some((rows, cols)) = get_terminal_size() {
                proxy.set_window_size(rows, cols).ok();
            }
            let status = if lock { "locked" } else { "unlocked" };
            event_log::push(format!("lock: shell respawned ({})", status));
            let msg = format!("\r\n{}Shell {}{}\r\n", display::GREEN, status, display::RESET);
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
        }
        Err(e) => {
            let msg = format!("\r\n{}Failed to respawn shell: {}{}\r\n", display::RED, e, display::RESET);
            nix::unistd::write(std::io::stdout(), msg.as_bytes()).ok();
        }
    }
}
```

- [ ] **Step 2: Update all `handle_lock` call sites**

Search call sites and add the `sandbox_backend` parameter:

```
handle_lock(&mut proxy, &mut master_fd, &mut locked, lock, &shell, &shell_args_ref, &session_id, sandbox_backend);
```

The `sandbox_backend` variable should be resolved once at startup (same as Task 7) and available in scope.

- [ ] **Step 3: Build**

Run: `cargo build --release 2>&1 | tail -10`
Expected: successful build.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "refactor(sandbox): migrate handle_lock to sandbox API"
```

---

### Task 9: Remove backward-compat shims and old code

**Files:**
- Modify: `crates/omnish-plugin/src/lib.rs`

- [ ] **Step 1: Remove shims from lib.rs**

After Tasks 7-8, no code calls `apply_sandbox()`, `apply_lock_sandbox()`, or `sandbox_profile()` on `omnish_plugin` directly. Remove all backward-compat shims from `lib.rs`. The file should be:

```rust
//! Shared plugin infrastructure: sandbox backends and tool implementations.

pub mod formatter;
pub mod sandbox;
pub mod tools;

pub use sandbox::{
    detect_backend, is_available, lock_policy, plugin_policy, sandbox_command, SandboxBackendType,
    SandboxPolicy,
};

/// Check whether Landlock is supported (used by event log / diagnostics).
pub fn is_landlock_supported() -> bool {
    is_available(SandboxBackendType::Landlock)
}
```

- [ ] **Step 2: Verify no remaining references to old functions**

Run: `grep -rn 'omnish_plugin::apply_sandbox\|omnish_plugin::apply_lock_sandbox\|omnish_plugin::sandbox_profile' crates/`
Expected: no matches.

- [ ] **Step 3: Build and test**

Run: `cargo build --release && cargo test -p omnish-plugin 2>&1 | tail -10`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-plugin/src/lib.rs
git commit -m "refactor(sandbox): remove backward-compat shims"
```

---

### Task 10: Manual integration test

**Files:** None (testing only)

- [ ] **Step 1: Test bwrap backend**

Verify bwrap is installed:

```bash
which bwrap
```

Test with a sandboxed command manually:

```bash
# Should succeed - /tmp is writable
bwrap --new-session --die-with-parent --ro-bind / / --dev /dev --bind /tmp /tmp -- touch /tmp/sandbox-test && echo "OK" && rm /tmp/sandbox-test

# Should fail - / is read-only
bwrap --new-session --die-with-parent --ro-bind / / --dev /dev -- touch /sandbox-test 2>&1 || echo "BLOCKED (expected)"
```

- [ ] **Step 2: Build release and verify omnish starts**

```bash
cargo build --release
```

Ask user to start omnish-daemon and test:
- Normal chat tool calls work with sandbox
- `/test lock on` applies sandbox
- `/test lock off` removes sandbox

- [ ] **Step 3: Verify config**

Test with `daemon.toml`:

```toml
[sandbox]
backend = "bwrap"
```

And also:

```toml
[sandbox]
backend = "landlock"
```

Both should work. Default (no `backend` key) should use bwrap.

- [ ] **Step 4: Final commit with spec update**

```bash
git add docs/plans/
git commit -m "docs: add sandbox backend abstraction spec and plan"
```
