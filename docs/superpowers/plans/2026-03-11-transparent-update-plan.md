# `/update` Transparent Client Self-Restart - Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/update` command that re-execs the omnish-client process, preserving the PTY connection to the child bash process.

**Architecture:** The client detects `/update` in chat/interceptor command dispatch, runs the on-disk binary with `--version` to compare versions, clears FD_CLOEXEC on the PTY master fd, then calls `execvp` with `--resume --fd=N --pid=P --session-id=S` args. On startup, `--resume` skips fork/spawn and reconstructs `PtyProxy` from the passed fd/pid.

**Tech Stack:** Rust, nix crate (fcntl, execvp), libc (FD_CLOEXEC)

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/omnish-pty/src/proxy.rs` | Modify | Add `PtyProxy::from_raw_fd(fd, pid)` constructor |
| `crates/omnish-client/src/command.rs` | Modify | Register `/update` command entry |
| `crates/omnish-client/src/main.rs` | Modify | Parse `--resume` args at startup; intercept `/update` and exec |

---

### Task 1: Add `PtyProxy::from_raw_fd` constructor

**Files:**
- Modify: `crates/omnish-pty/src/proxy.rs`

- [ ] **Step 1: Add `from_raw_fd` method**

In `crates/omnish-pty/src/proxy.rs`, add after `spawn_with_env`:

```rust
/// Reconstruct a PtyProxy from an existing master fd and child pid.
/// Used for resuming after exec (the fd and child survive the exec boundary).
///
/// # Safety
/// The caller must ensure `fd` is a valid open PTY master file descriptor
/// and `pid` is a valid child process ID.
pub unsafe fn from_raw_fd(fd: RawFd, pid: i32) -> Self {
    PtyProxy {
        master_fd: OwnedFd::from_raw_fd(fd),
        child_pid: Pid::from_raw(pid),
    }
}
```

This requires adding `FromRawFd` to the imports:
```rust
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
```

- [ ] **Step 2: Build to verify**

Run: `cargo build -p omnish-pty`
Expected: success

- [ ] **Step 3: Commit**

```
git add crates/omnish-pty/src/proxy.rs
git commit -m "feat(pty): add PtyProxy::from_raw_fd for exec resume"
```

---

### Task 2: Register `/update` command

**Files:**
- Modify: `crates/omnish-client/src/command.rs`

- [ ] **Step 1: Add `/update` to COMMANDS array**

In `crates/omnish-client/src/command.rs`, add to the `COMMANDS` array (after `/tasks`):

```rust
CommandEntry {
    path: "/update",
    kind: CommandKind::Daemon("__cmd:update"),
    help: "Re-exec client from updated binary on disk",
},
```

Use `Daemon` kind so it reaches `handle_slash_command` in main.rs, where it will be intercepted client-side (same pattern as `/debug client`).

- [ ] **Step 2: Build to verify**

Run: `cargo build -p omnish-client`
Expected: success

- [ ] **Step 3: Commit**

```
git add crates/omnish-client/src/command.rs
git commit -m "feat(client): register /update command"
```

---

### Task 3: Implement `--resume` startup path and `/update` exec logic

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

- [ ] **Step 1: Parse `--resume` arguments at startup**

At the top of `async fn main()`, after the `--version` check (line 121-124), add resume argument parsing:

```rust
// Check for --resume mode (re-exec from /update)
let resume_args = parse_resume_args();
```

Add the parsing function near the top of the file (after the helper functions, before `main`):

```rust
struct ResumeArgs {
    master_fd: i32,
    child_pid: i32,
    session_id: String,
}

fn parse_resume_args() -> Option<ResumeArgs> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--resume") {
        return None;
    }
    let fd = args.iter()
        .find_map(|a| a.strip_prefix("--fd="))
        .and_then(|v| v.parse::<i32>().ok())?;
    let pid = args.iter()
        .find_map(|a| a.strip_prefix("--pid="))
        .and_then(|v| v.parse::<i32>().ok())?;
    let sid = args.iter()
        .find_map(|a| a.strip_prefix("--session-id="))?
        .to_string();
    Some(ResumeArgs { master_fd: fd, child_pid: pid, session_id: sid })
}
```

- [ ] **Step 2: Branch startup on resume vs normal**

Replace the section from `let session_id = ...` through `let proxy = PtyProxy::spawn_with_env(...)` (lines 142-164) with a branch:

```rust
let (session_id, proxy) = if let Some(ref resume) = resume_args {
    // Resume mode: reconstruct PtyProxy from passed fd/pid
    let proxy = unsafe { PtyProxy::from_raw_fd(resume.master_fd, resume.child_pid) };
    eprintln!("\x1b[32m[omnish]\x1b[0m Resumed (pid={}, fd={})", resume.child_pid, resume.master_fd);
    (resume.session_id.clone(), proxy)
} else {
    // Normal startup: spawn a new shell
    let session_id = Uuid::new_v4().to_string()[..8].to_string();
    let shell = resolve_shell(&config.shell.command);

    let mut child_env = HashMap::new();
    child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
    child_env.insert("SHELL".to_string(), shell.clone());

    let osc133_rcfile = shell_hook::install_bash_hook(&shell);
    let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
        vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
    } else {
        vec![]
    };
    let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();
    let proxy = PtyProxy::spawn_with_env(&shell, &shell_args_ref, child_env)?;
    (session_id, proxy)
};

let parent_session_id = std::env::var("OMNISH_SESSION_ID").ok();
```

Note: `osc133_hook_installed` is used later in the code. In resume mode it should be `true` (the shell already has hooks from the previous client). We need to handle this:

```rust
let osc133_hook_installed = if resume_args.is_some() {
    true // Shell hooks persist across client re-exec
} else {
    // Already set during spawn above - but we moved the code into the else branch.
    // We need to track this. See the full restructure below.
    ...
};
```

Actually, let's restructure to keep it clean. The full replacement of lines 142-168 should be:

```rust
let (session_id, proxy, osc133_hook_installed) = if let Some(ref resume) = resume_args {
    let proxy = unsafe { PtyProxy::from_raw_fd(resume.master_fd, resume.child_pid) };
    eprintln!("\x1b[32m[omnish]\x1b[0m Resumed (pid={}, fd={})", resume.child_pid, resume.master_fd);
    (resume.session_id.clone(), proxy, true)
} else {
    let session_id = Uuid::new_v4().to_string()[..8].to_string();
    let shell = resolve_shell(&config.shell.command);

    let mut child_env = HashMap::new();
    child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
    child_env.insert("SHELL".to_string(), shell.clone());

    let osc133_rcfile = shell_hook::install_bash_hook(&shell);
    let osc133_hook_installed = osc133_rcfile.is_some();
    let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
        vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
    } else {
        vec![]
    };
    let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();
    let proxy = PtyProxy::spawn_with_env(&shell, &shell_args_ref, child_env)?;
    (session_id, proxy, osc133_hook_installed)
};
let parent_session_id = std::env::var("OMNISH_SESSION_ID").ok();
let daemon_addr = std::env::var("OMNISH_SOCKET")
    .unwrap_or_else(|_| config.daemon_addr.clone());
```

- [ ] **Step 3: Implement `/update` exec logic**

Add the exec function near `parse_resume_args`:

```rust
fn exec_update(proxy: &PtyProxy, session_id: &str) {
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\x1b[31m[omnish]\x1b[0m Failed to resolve current exe: {}", e);
            return;
        }
    };

    if !current_exe.exists() {
        eprintln!("\x1b[31m[omnish]\x1b[0m Binary not found: {}", current_exe.display());
        return;
    }

    // Get on-disk binary version by running it with --version
    let disk_version = match std::process::Command::new(&current_exe)
        .arg("--version")
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => {
            eprintln!("\x1b[31m[omnish]\x1b[0m Failed to check binary version: {}", e);
            return;
        }
    };

    let running_version = format!("omnish {}", omnish_common::VERSION);
    if disk_version == running_version {
        eprintln!("\x1b[33m[omnish]\x1b[0m Already up to date ({})", omnish_common::VERSION);
        return;
    }

    eprintln!(
        "\x1b[32m[omnish]\x1b[0m Updating: {} -> {}",
        running_version, disk_version
    );

    // Clear FD_CLOEXEC on the PTY master fd so it survives exec
    let master_fd = proxy.master_raw_fd();
    unsafe {
        let flags = libc::fcntl(master_fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(master_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }

    // Build args for the new process
    let exe_cstr = std::ffi::CString::new(current_exe.to_string_lossy().as_bytes()).unwrap();
    let args = [
        exe_cstr.clone(),
        std::ffi::CString::new("--resume").unwrap(),
        std::ffi::CString::new(format!("--fd={}", master_fd)).unwrap(),
        std::ffi::CString::new(format!("--pid={}", proxy.child_pid())).unwrap(),
        std::ffi::CString::new(format!("--session-id={}", session_id)).unwrap(),
    ];

    // execvp replaces this process - only returns on error
    let _ = nix::unistd::execvp(&exe_cstr, &args);
    eprintln!("\x1b[31m[omnish]\x1b[0m exec failed: {}", std::io::Error::last_os_error());
}
```

- [ ] **Step 4: Intercept `/update` in `handle_slash_command`**

In `handle_slash_command` (around line 1496), add an intercept before the `command::dispatch` call, similar to the `/debug client` pattern:

```rust
async fn handle_slash_command(
    trimmed: &str,
    session_id: &str,
    rpc: &RpcClient,
    proxy: &PtyProxy,
    client_debug_fn: &dyn Fn() -> String,
) -> bool {
    // Intercept /update client-side (needs process state: proxy fd/pid)
    if trimmed == "/update" {
        exec_update(proxy, session_id);
        return true; // Only reached if exec failed
    }

    match command::dispatch(trimmed) {
        ...
```

- [ ] **Step 5: Build and verify**

Run: `cargo build --workspace`
Expected: success

- [ ] **Step 6: Commit**

```
git add crates/omnish-client/src/main.rs
git commit -m "feat(client): implement /update transparent self-restart (issue #217)"
```

---

### Task 4: Manual testing

- [ ] **Step 1: Build and install**

```
cargo build --workspace
```

- [ ] **Step 2: Test same-version case**

Launch omnish, type `/update`. Expected output:
```
[omnish] Already up to date (0.5.5)
```

- [ ] **Step 3: Test resume flag directly**

From inside omnish, note the master_fd and child_pid from `/debug client`, then in another terminal:
```
# Just verify the args parse correctly - this is a smoke test
target/debug/omnish-client --resume --fd=999 --pid=999 --session-id=test1234
```
Expected: it should try to resume and fail gracefully (fd 999 is invalid).

- [ ] **Step 4: Test full exec cycle**

1. Build omnish, launch it
2. Change VERSION in Cargo.toml (e.g., 0.5.5 -> 0.5.6)
3. Rebuild: `cargo build -p omnish-client`
4. In the running omnish session, type `/update`
5. Expected: prints "Updating: omnish 0.5.5 -> omnish 0.5.6", session continues, shell is uninterrupted
6. Type `/debug client` - should show Version: omnish 0.5.6

- [ ] **Step 5: Push and close issue**

```
git push
glab issue note 217 -m "实现了 /update 命令..."
glab issue close 217
```
