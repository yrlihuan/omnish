# macOS sandbox-exec Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add macOS filesystem sandboxing for plugin subprocesses using `sandbox-exec`, matching the existing Linux Landlock policy.

**Architecture:** On macOS, instead of applying a sandbox inside `pre_exec`, wrap the plugin executable with `sandbox-exec -p '<profile>'`. The `.sb` profile is built dynamically with the same writable paths as Landlock (data_dir, /tmp, /dev/null, cwd, git repo root). Platform-specific code is separated with `#[cfg]` gates.

**Tech Stack:** Rust, macOS `sandbox-exec` CLI, Scheme-based `.sb` profile format. No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-03-19-macos-sandbox-exec-design.md`

---

## Chunk 1: Plugin Library Changes

### Task 1: Make `git_repo_root()` cross-platform

**Files:**
- Modify: `crates/omnish-plugin/src/lib.rs:15-25`

- [ ] **Step 1: Remove the Linux-only cfg gate from `git_repo_root()`**

Change line 15 from `#[cfg(target_os = "linux")]` to no cfg gate. The function is a plain `git rev-parse` call with no platform-specific code. It's called by `apply_sandbox()` on Linux and will be called by `sandbox_profile()` on macOS.

```rust
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p omnish-plugin`
Expected: no errors (the function is `fn`, not `pub fn`, so no unused warning on macOS since `apply_sandbox` on Linux uses it, and `sandbox_profile` on macOS will use it)

Note: On Linux where we're building, clippy may warn about unused function since `sandbox_profile` doesn't exist on Linux. To fix this, add `#[cfg(any(target_os = "linux", target_os = "macos"))]` instead of removing the gate entirely. However, since we always build on Linux, and the function is used by `apply_sandbox()` on Linux, no warning will appear. If cross-compiling for other targets (e.g., Windows), the warning would appear but we don't support Windows.

Actually, the simplest approach: keep the function ungated. On Linux it's used by `apply_sandbox()`. On macOS it will be used by `sandbox_profile()`. On other platforms it's unused but that's fine - we only target Linux and macOS.

- [ ] **Step 3: Commit**

```bash
git add crates/omnish-plugin/src/lib.rs
git commit -m "refactor: make git_repo_root() cross-platform for macOS sandbox support"
```

---

### Task 2: Add `sandbox_profile()` function for macOS

**Files:**
- Modify: `crates/omnish-plugin/src/lib.rs`

- [ ] **Step 1: Write the test for `sandbox_profile()`**

Add a `#[cfg(test)]` module at the bottom of `lib.rs`. Since `sandbox_profile()` is `#[cfg(target_os = "macos")]`, we can't test it directly on Linux. Instead, extract the profile-building logic into an always-available helper `build_sandbox_profile()` and have `sandbox_profile()` call it. This lets us test the core logic on any platform.

Add these functions and tests to `crates/omnish-plugin/src/lib.rs`:

```rust
/// Escape a path string for use inside a sandbox-exec `.sb` profile.
/// Escapes backslashes first, then double quotes, to prevent profile injection.
fn escape_sb_path(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build a sandbox-exec `.sb` profile string.
/// Policy: deny all by default, allow all non-file-write operations,
/// allow file reads everywhere, allow file writes only to specified paths.
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

    // data_dir is always present
    let escaped = escape_sb_path(&data_dir.to_string_lossy());
    profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));

    if let Some(cwd) = cwd {
        let escaped = escape_sb_path(&cwd.to_string_lossy());
        profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
    }

    if let Some(root) = repo_root {
        let escaped = escape_sb_path(&root.to_string_lossy());
        profile.push_str(&format!("(allow file-write* (subpath \"{escaped}\"))\n"));
    }

    profile
}
```

Then add tests:

```rust
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
        // Backslash must be escaped first, then quote
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
        // No cwd or repo root lines
        assert_eq!(profile.matches("(allow file-write*").count(), 3);
    }

    #[test]
    fn test_build_sandbox_profile_with_cwd_and_repo() {
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            Some(Path::new("/home/user/project")),
            Some(Path::new("/home/user/project")),
        );
        assert!(profile.contains("(allow file-write* (subpath \"/home/user/project\"))"));
        // data_dir + /tmp + /dev/null + cwd + repo = 5 file-write rules
        assert_eq!(profile.matches("(allow file-write*").count(), 5);
    }

    #[test]
    fn test_build_sandbox_profile_with_cwd_only() {
        let profile = build_sandbox_profile(
            Path::new("/data/plugin"),
            Some(Path::new("/work")),
            None,
        );
        // data_dir + /tmp + /dev/null + cwd = 4 file-write rules
        assert_eq!(profile.matches("(allow file-write*").count(), 4);
        assert!(profile.contains("(allow file-write* (subpath \"/work\"))"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p omnish-plugin -- tests`
Expected: FAIL - `escape_sb_path` and `build_sandbox_profile` don't exist yet

- [ ] **Step 3: Add `escape_sb_path()` and `build_sandbox_profile()` implementations**

Add the two functions (shown in Step 1 above) to `crates/omnish-plugin/src/lib.rs`, after the `git_repo_root()` function and before the `apply_sandbox()` functions.

- [ ] **Step 4: Add the macOS-only `sandbox_profile()` public function**

Add after `build_sandbox_profile()`:

```rust
/// Build a sandbox-exec `.sb` profile for macOS.
/// Computes git repo root from `cwd` (if provided) and delegates to `build_sandbox_profile()`.
#[cfg(target_os = "macos")]
pub fn sandbox_profile(data_dir: &std::path::Path, cwd: Option<&std::path::Path>) -> String {
    let repo_root = cwd.and_then(git_repo_root);
    build_sandbox_profile(data_dir, cwd, repo_root.as_deref())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p omnish-plugin -- tests`
Expected: all 6 tests PASS

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p omnish-plugin`
Expected: no warnings

- [ ] **Step 7: Commit**

```bash
git add crates/omnish-plugin/src/lib.rs
git commit -m "feat: add sandbox_profile() for macOS sandbox-exec support (#345)"
```

---

## Chunk 2: Client Plugin Manager Changes

### Task 3: Use `sandbox-exec` on macOS in `execute_tool()`

**Files:**
- Modify: `crates/omnish-client/src/client_plugin.rs:74-91`

- [ ] **Step 1: Gate `CommandExt` import to Linux only**

In `client_plugin.rs` line 5, change:
```rust
use std::os::unix::process::CommandExt;
```
to:
```rust
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
```

`CommandExt` provides `pre_exec()` which is only called in the `#[cfg(target_os = "linux")]` block. Without this gate, macOS builds get an unused import warning.

- [ ] **Step 2: Restructure command construction with platform-specific sandbox logic**

Replace lines 74-91 in `client_plugin.rs` with platform-aware command construction:

```rust
        let cwd_path: Option<std::path::PathBuf> = cwd.map(std::path::PathBuf::from);

        // On macOS: wrap with sandbox-exec; on Linux: use pre_exec Landlock
        #[cfg(target_os = "macos")]
        let mut cmd = if sandboxed {
            let mut c = Command::new("sandbox-exec");
            let profile = omnish_plugin::sandbox_profile(
                &data_dir,
                cwd_path.as_deref(),
            );
            c.args([
                "-p",
                &profile,
                &executable.to_string_lossy(),
            ]);
            c
        } else {
            Command::new(&executable)
        };

        #[cfg(not(target_os = "macos"))]
        let mut cmd = Command::new(&executable);

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        // Apply Landlock sandbox via pre_exec on Linux
        #[cfg(target_os = "linux")]
        if sandboxed {
            let data_dir_clone = data_dir.clone();
            let cwd_clone = cwd_path.clone();
            unsafe {
                cmd.pre_exec(move || {
                    omnish_plugin::apply_sandbox(&data_dir_clone, cwd_clone.as_deref()).map_err(
                        |e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
                    )
                });
            }
        }
```

This replaces the current code at lines 74-91. Key changes:
- On macOS + sandboxed: `Command::new("sandbox-exec")` with profile and executable as args
- On Linux + sandboxed: existing `pre_exec` Landlock path (unchanged logic)
- On other platforms: `Command::new(&executable)` with no sandbox (existing no-op)
- `cwd_path` is extracted earlier since it's needed for both the profile and the pre_exec closure

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p omnish-client`
Expected: no errors

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 5: Run clippy on both crates**

Run: `cargo clippy -p omnish-plugin -p omnish-client`
Expected: no warnings

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/client_plugin.rs
git commit -m "feat: use sandbox-exec on macOS for plugin sandboxing (#345)"
```

---

## Chunk 3: Cleanup and Documentation

### Task 4: Update the non-Linux no-op sandbox comment

**Files:**
- Modify: `crates/omnish-plugin/src/lib.rs:66-70`

- [ ] **Step 1: Update the no-op comment**

The current `#[cfg(not(target_os = "linux"))]` no-op `apply_sandbox()` comment says "No-op sandbox on non-Linux platforms." This is now misleading since macOS has its own sandbox path (at the command level). Update:

```rust
/// No-op: on macOS, sandboxing is applied at the command level via sandbox-exec.
/// On other non-Linux platforms, sandboxing is not available.
#[cfg(not(target_os = "linux"))]
pub fn apply_sandbox(_data_dir: &std::path::Path, _cwd: Option<&std::path::Path>) -> Result<(), String> {
    Ok(())
}
```

- [ ] **Step 2: Commit all changes**

```bash
git add crates/omnish-plugin/src/lib.rs
git commit -m "docs: update no-op sandbox comment for macOS clarity (#345)"
```

### Task 5: Final verification

- [ ] **Step 1: Full build**

Run: `cargo build --workspace`
Expected: success

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 3: Clippy clean**

Run: `cargo clippy --workspace`
Expected: no warnings
