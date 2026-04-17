# macOS sandbox-exec Support for Plugin Processes

**Issue**: #345
**Date**: 2026-03-19
**Status**: Design

## Problem

The plugin subprocess sandbox (`apply_sandbox()`) uses Landlock on Linux but is a no-op on macOS. Plugin processes on macOS run with full filesystem write access.

## Goal

Implement equivalent filesystem sandboxing on macOS using `sandbox-exec`, matching the Landlock policy: read everywhere, write only to `data_dir`, `/tmp`, `/dev/null`, cwd, and git repo root.

## Approach

Use `sandbox-exec -p '<profile>'` to wrap the plugin executable. Instead of applying the sandbox inside `pre_exec` (as Landlock does), change the command invocation on macOS to launch `sandbox-exec` as the top-level process with an inline `.sb` profile string.

### Sandbox Profile

```scheme
(version 1)
(deny default)
(allow process*)
(allow signal)
(allow sysctl*)
(allow mach*)
(allow ipc*)
(allow network*)
(allow file-read*)
(allow file-write* (subpath "/tmp"))
(allow file-write* (literal "/dev/null"))
(allow file-write* (subpath "<data_dir>"))
(allow file-write* (subpath "<cwd>"))          ; omitted if cwd is None
(allow file-write* (subpath "<git_repo_root>")) ; omitted if not in a repo
```

Uses `(deny default)` with explicit allows for each operation class. This is unambiguous regardless of the sandbox profile evaluation model (most-specific-match vs first-match). All non-filesystem operations are permitted (process, signal, network, mach IPC, sysctl). File reads are allowed globally. File writes are only allowed for specific paths, matching the Landlock policy.

**Debugging sandbox violations**: Violations are logged to the system log, not stderr. Use `log show --predicate 'process == "sandbox-exec"'` to inspect.

### Code Changes

#### `crates/omnish-plugin/src/lib.rs`

- **`git_repo_root()`**: Remove `#[cfg(target_os = "linux")]` gate. The function is a plain `git rev-parse` call with no platform-specific code. Needed on both Linux and macOS now.

- **New function** `sandbox_profile(data_dir, cwd) -> String` (`#[cfg(target_os = "macos")]`):
  - Builds the `.sb` profile string with dynamic paths
  - Calls `git_repo_root(cwd)` to optionally include the repo root
  - Sanitizes paths: escapes `\` then `"` in path values to prevent profile injection
  - Omits `cwd` and `git_repo_root` lines when those values are `None`

- **`apply_sandbox()`**: Remains `#[cfg(target_os = "linux")]` for Landlock. The macOS no-op variant is kept but never called in the sandboxed path (sandboxing happens at command level).

#### `crates/omnish-client/src/client_plugin.rs`

- **Sandboxed command construction** (macOS): When `sandboxed` is true on macOS, instead of `Command::new(&executable)`, use:
  ```rust
  let profile = omnish_plugin::sandbox_profile(&data_dir, cwd_path);
  cmd = Command::new("sandbox-exec");
  cmd.args(["-p", &profile, executable.to_str().unwrap()]);
  ```
  The `pre_exec` Landlock hook is skipped on macOS.

- **Sandboxed command construction** (Linux): Unchanged - `Command::new(&executable)` with `pre_exec` calling `apply_sandbox()`.

- Platform separation via `#[cfg(target_os = "...")]` blocks within `execute_tool()`.

### Error Handling

- **`sandbox-exec` not found**: `Command::new("sandbox-exec").spawn()` fails naturally with the existing error path (`"Failed to spawn plugin '...': ..."`). No special handling needed.

- **Git repo root**: Computed in the parent process before spawning, so no sandbox restrictions apply to the detection.

- **Path sanitization**: Backslashes are escaped first, then double quotes, to prevent `.sb` profile injection via crafted path names.

- **No cwd**: When `cwd` is `None`, the corresponding `(allow file-write*)` line is omitted from the profile. Same for git repo root when not in a repo.

### No New Dependencies

`sandbox-exec` is a macOS system binary. The profile is a plain string built in Rust. No new crates needed.

## Deprecation Risk

`sandbox-exec` has been marked deprecated since macOS 10.15 but remains functional on current macOS versions (including macOS 15). Many widely-used tools (Homebrew, Nix, Bazel) continue to rely on it. The risk of removal is low in the near term, and there is no practical alternative for userspace process sandboxing on macOS without code signing entitlements.

If Apple removes `sandbox-exec` in a future release, the fallback is the current no-op behavior - plugins run unsandboxed, same as today. This is graceful degradation, not a security regression.
