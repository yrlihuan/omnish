# `/update` - Transparent Client Self-Restart

## Problem

When the omnish-client binary is updated on disk (e.g., `cargo install`), the running client is still the old version. Restarting means killing the child bash process and losing the session.

## Solution

`/update` re-execs the client process using the binary currently on disk. The PTY master fd and child pid survive the exec boundary. The child bash process is undisturbed.

## Mechanism

1. User types `/update` at the shell prompt
2. Client resolves `current_exe()`, reads its version, compares to running version
3. If same version: print "already up to date", return
4. If different: print version info, exec into new binary
5. Steps before exec:
   - Clear `FD_CLOEXEC` on PTY master fd (exec closes CLOEXEC fds by default)
   - Call `execvp(current_exe, ["omnish-client", "--resume", "--fd={master_fd}", "--pid={child_pid}", "--session-id={session_id}"])`
6. New process detects `--resume`:
   - Reconstructs `PtyProxy` from fd + pid (no fork/spawn)
   - Enters raw mode
   - Connects to daemon with same session_id
   - Enters normal event loop

## Components

### `command.rs`
Register `/update` command. Uses `Daemon` kind but intercepted client-side in `main.rs` (same pattern as `/debug client`) since it needs process state.

### `main.rs`
- Parse `--resume`, `--fd`, `--pid`, `--session-id` arguments at startup
- In resume mode: skip fork/spawn, reconstruct PtyProxy from args
- Intercept `/update` in `handle_slash_command`: version check + exec

### `proxy.rs`
Add `PtyProxy::from_raw(fd: RawFd, pid: i32)` - wraps existing fd+pid without spawning.

## State Across Exec

| Survives | Rebuilt |
|----------|---------|
| PTY master fd | RawModeGuard (re-entered) |
| Child pid | Daemon connection (reconnected) |
| Session ID | ShellInputTracker, CommandTracker |
| Terminal raw state (kernel) | GhostCompleter, InputInterceptor |
| | Probe polling task |

## Edge Cases

- **Same version**: print "already up to date", no exec
- **Binary not found**: print error, continue
- **Exec fails**: execvp only returns on error - print error, continue old version
- **Not at prompt**: `/update` only reachable at prompt (interceptor guard)
