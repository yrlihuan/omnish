# Zsh Shell Hook Support

**Issue:** #462 - 测试zsh支持
**Date:** 2026-04-13

## Goal

Implement zsh shell hooks with full feature parity to the existing bash hooks (OSC 133 A/B/C/D + readline reporting), and parameterize the integration test suite to run under both bash and zsh with a separate CI job.

## Design

### 1. Zsh Hook Implementation

A `ZSH_HOOK` constant in `shell_hook.rs`, emitting the same OSC 133 sequences as `BASH_HOOK`:

**precmd hook** (replaces bash's `PROMPT_COMMAND`):
- Captures `$?` exit code first
- Emits `133;D;{exit_code}` (CommandEnd) then `133;A` (PromptStart)
- Appends to `precmd_functions` array (not replace) for coexistence with oh-my-zsh/prezto/p10k

**preexec hook** (replaces bash's `DEBUG trap`):
- Receives the command string as `$1` (zsh passes it natively)
- Emits `133;B;{cmd};cwd:{pwd};orig:{orig}` (CommandStart)
- Uses `fc -ln -1` for original input
- Semicolons escaped as `\;`, newlines escaped

**ZLE widget for readline** (replaces bash's `bind -x`):
- Custom widget `__omnish-rl-report` bound to `\e[13337~`
- Reads `$BUFFER` (= `READLINE_LINE`) and `$CURSOR` (= `READLINE_POINT`)
- Emits `133;RL;{buffer};{cursor}`

**Guard variables** (same pattern as bash):
- `__omnish_preexec_fired` - prevent double-fire
- `__omnish_in_precmd` - guard preexec during precmd

**Option isolation:**
- Hook functions wrapped with `emulate -L zsh` to isolate from user `setopt` settings

### 2. Hook Installation & Shell Detection

**Installation** (`shell_hook.rs`):
- Add `ZSH_HOOK` constant alongside existing `BASH_HOOK`
- `install_zsh_hook()`:
  - Writes hook to `~/.local/share/omnish/hooks/zsh_hook.zsh`
  - Writes wrapper to `~/.local/share/omnish/hooks/zshrc` that sources user's `.zshrc` then the hook

**Shell spawning** (`main.rs`):
- Detect shell type from resolved shell path (ends with `zsh` or `bash`)
- For bash: `--rcfile` (existing)
- For zsh: set `ZDOTDIR` env var to a temp directory containing a `.zshrc` that sources the wrapper
- Preserve original `ZDOTDIR` via `OMNISH_ORIG_ZDOTDIR` so user config is sourced from the right place
- For other shells: no hook, fallback to regex prompt detection (existing)

### 3. Test Parameterization

**lib.sh changes:**
- New env var `TEST_SHELL` (default: `bash`)
- tmux `default-shell` set to resolved shell path instead of hardcoded `/bin/bash`
- `is_shell_prompt()` updated to match zsh prompts (`%` suffix)
- Helper `require_shell()` to skip test run if shell not installed

**Test execution:**
- `TEST_SHELL=bash bash test_basic.sh` - existing behavior
- `TEST_SHELL=zsh bash test_basic.sh` - same tests under zsh
- Tests that are truly bash-only guarded with `[[ $TEST_SHELL == bash ]] || return 0`

### 4. CI Pipeline

- Existing integration test job unchanged (bash, default)
- New job `integration-test-zsh`: installs zsh, runs same test scripts with `TEST_SHELL=zsh`
- Separate job so bash tests aren't blocked by zsh issues

## Edge Cases

- **oh-my-zsh / prezto / p10k**: Using `precmd_functions`/`preexec_functions` arrays ensures coexistence
- **p10k instant prompt**: Our hook appends after sourcing user `.zshrc`, runs after p10k's precmd
- **`ZDOTDIR` already set**: Wrapper sources `${OMNISH_ORIG_ZDOTDIR:-$HOME}/.zshrc`
- **Multiline commands**: Escape `\n` in OSC payload
- **`setopt` interactions**: `emulate -L zsh` isolates option state

## Out of Scope

- Fish shell support
- Zsh-specific features beyond bash parity (e.g. ZLE push mode)
- Zsh completion/plugin framework integration
