# Shell Prompt State Tracking

## Overview

The client needs to know whether the user is **at the shell prompt** (typing a command) or **not** (command executing, output streaming, etc.) to decide when to trigger LLM auto-completion.

This state is tracked by `ShellInputTracker.at_prompt` in `crates/omnish-client/src/shell_input.rs`.

## OSC 133 Semantic Prompt Protocol

Shells can emit OSC 133 escape sequences to mark prompt boundaries:

| Sequence | Enum Variant   | Meaning                          |
|----------|----------------|----------------------------------|
| `133;A`  | `PromptStart`  | Shell is about to draw the prompt |
| `133;B`  | `CommandStart` | Prompt done, command input area   |
| `133;C`  | `OutputStart`  | Command output begins             |
| `133;D`  | `CommandEnd`   | Command finished (with exit code) |

## Bash Hook Implementation

The omnish bash hook (`crates/omnish-client/src/shell_hook.rs`) uses two mechanisms:

```bash
# Fires when shell displays a prompt
PROMPT_COMMAND → emits 133;D then 133;A

# Fires before each command execution (via DEBUG trap)
__omnish_preexec → emits 133;B then 133;C
```

### Critical detail: DEBUG trap and PS1

The bash `DEBUG` trap fires before **every** simple command, including commands inside PS1 evaluation (e.g. `$(git branch --show-current)`). This means `133;B` + `133;C` fire **during prompt display**, not when the user presses Enter.

Actual event sequence when prompt is displayed:

```
133;D (CommandEnd)     ← PROMPT_COMMAND
133;A (PromptStart)    ← PROMPT_COMMAND
133;B (CommandStart)   ← DEBUG trap from PS1 command substitution
133;C (OutputStart)    ← DEBUG trap from PS1 command substitution
<prompt text appears>
<user types here>
```

## State Transition Design

Because `133;B` and `133;C` fire unreliably (during PS1 evaluation, not just on user Enter), we use a hybrid approach:

### `at_prompt = true` triggers

- **OSC 133;A** (`PromptStart`) — shell is drawing a new prompt
- **OSC 133;D** (`CommandEnd`) — previous command finished

Both are emitted from `PROMPT_COMMAND`, which runs reliably only when the shell is ready for user input.

### `at_prompt = false` trigger

- **Enter key** (0x0d / 0x0a) in `feed_forwarded()` — the user submitted a command

This is detected from user keyboard input, not from OSC events.

### Ignored for `at_prompt`

- **OSC 133;B** (`CommandStart`) — unreliable due to PS1 DEBUG trap
- **OSC 133;C** (`OutputStart`) — unreliable, same reason

These events still trigger `shell_completer.clear()` to dismiss any visible ghost text.

## CSI Trigger Guard (commit 28820fc)

The client sends a CSI sequence `\x1b[13337~` to trigger bash's READLINE_PROMPT_STARTED hook, which allows reading the actual input line. However, this escape sequence could appear as raw characters `^[[13337~` when the bash hook is not installed or when not at the prompt.

To prevent this, the CSI trigger is now guarded by two conditions:

1. **For Up/Down arrow navigation**: Only send CSI trigger when `osc133_hook_installed` is true
2. **For completion responses**: Only send CSI trigger when both `osc133_hook_installed` is true AND `shell_input.at_prompt()` returns true

This fix resolves issue #27 where raw escape sequences appeared in the terminal.

## CWD Tracking via ShellCwdProbe

Traditional shell tracking relies on `$PWD` environment variable, but this can become stale or incorrect when:
- The shell changes directory without updating PWD (e.g., `cd` in a subshell)
- Symbolic links are involved
- The client process has a different working directory than the shell

Omnish now uses `ShellCwdProbe` to periodically poll the shell process's actual working directory by reading `/proc/{shell_pid}/cwd`:

```rust
pub struct ShellCwdProbe(pub u32);
impl Probe for ShellCwdProbe {
    fn key(&self) -> &str { "shell_cwd" }
    fn collect(&self) -> Option<String> {
        std::fs::read_link(format!("/proc/{}/cwd", self.0))
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    }
}
```

This probe is included in `default_polling_probes()` and runs alongside `ChildProcessProbe` to track:
- `shell_cwd` — The shell's actual working directory
- `child_process` — The currently running foreground process (name:pid)

This provides more accurate context for the LLM about where commands are executing.

## State Diagram

```
                    ┌─────────────────────────┐
                    │                         │
                    ▼                         │
              ┌──────────┐            ┌───────┴──────┐
              │ at_prompt│            │  !at_prompt   │
              │  = true  │───Enter───▶│  (command     │
              │ (typing) │   (0x0d)   │  executing)   │
              └──────────┘            └───────┬───────┘
                    ▲                         │
                    │     133;A or 133;D      │
                    └─────────────────────────┘
```

## Multi-line Input Behavior

When the user types a multi-line command (unclosed quotes, trailing `\`, heredoc, etc.):

1. First Enter → `at_prompt = false`, input cleared
2. Shell shows continuation prompt (PS2) — **no** 133;A/D emitted
3. `at_prompt` stays `false` during continuation lines
4. Command eventually executes and finishes
5. 133;D / 133;A → `at_prompt = true`

**Consequence:** Auto-completion does not trigger during multi-line input. This is acceptable for v1.

## Input Tracking Summary

| User Action         | `at_prompt` | Input Tracked? | Completion? |
|---------------------|-------------|----------------|-------------|
| Typing at prompt    | true        | Yes            | Yes (after 500ms debounce, >= 2 chars) |
| Multi-line cont.    | false       | No             | No          |
| Command running     | false       | No             | No          |
| New prompt appears  | true        | Yes            | Yes         |

## Related Files

- `crates/omnish-client/src/shell_input.rs` — `ShellInputTracker` implementation
- `crates/omnish-client/src/shell_hook.rs` — Bash hook that emits OSC 133
- `crates/omnish-client/src/completion.rs` — `ShellCompleter` debounce and ghost text
- `crates/omnish-client/src/main.rs` — OSC 133 event dispatch and completion request loop
- `crates/omnish-tracker/src/osc133_detector.rs` — OSC 133 sequence parser
