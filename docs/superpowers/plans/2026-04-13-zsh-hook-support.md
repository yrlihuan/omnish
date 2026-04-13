# Zsh Shell Hook Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add zsh shell hooks with full OSC 133 feature parity (A/B/C/D + readline reporting) and parameterize integration tests to run under both bash and zsh.

**Architecture:** Add `ZSH_HOOK` constant and `install_zsh_hook()` in `shell_hook.rs`, update shell spawning in `main.rs` to use `ZDOTDIR` for zsh, parameterize `lib.sh` with `TEST_SHELL` env var, add separate CI job.

**Tech Stack:** Rust (omnish-client), zsh (precmd/preexec/ZLE), bash (test framework), GitLab CI

---

## File Structure

- **Modify:** `crates/omnish-client/src/shell_hook.rs` — add `ZSH_HOOK` constant and `install_zsh_hook()`
- **Modify:** `crates/omnish-client/src/main.rs` — shell type detection, zsh hook installation, `ZDOTDIR` env setup
- **Modify:** `tools/integration_tests/lib.sh` — `TEST_SHELL` parameterization, zsh prompt detection
- **Modify:** `.gitlab-ci.yml` — new `integration-test-zsh` job

---

### Task 1: Add ZSH_HOOK constant to shell_hook.rs

**Files:**
- Modify: `crates/omnish-client/src/shell_hook.rs`

- [ ] **Step 1: Add the ZSH_HOOK constant**

Add below `BASH_HOOK` in `shell_hook.rs`:

```rust
const ZSH_HOOK: &str = r#"
# omnish shell integration — OSC 133 semantic prompts for zsh
emulate -L zsh

__omnish_preexec_fired=0
__omnish_in_precmd=0

__omnish_precmd() {
  emulate -L zsh
  local ec=$?
  __omnish_in_precmd=1
  __omnish_preexec_fired=0
  printf '\033]133;D;%d\007' "$ec"
  printf '\033]133;A\007'
  __omnish_in_precmd=0
}

__omnish_preexec() {
  emulate -L zsh
  [[ "$__omnish_in_precmd" == "1" ]] && return
  [[ "$__omnish_preexec_fired" == "1" ]] && return
  __omnish_preexec_fired=1
  # $1 is the full command string (zsh passes it natively)
  local cmd_esc="${1//;/\\;}"
  cmd_esc="${cmd_esc//$'\n'/\\n}"
  local pwd_esc="${PWD//;/\\;}"
  # Original input from history (preserves aliases unexpanded)
  local orig_esc
  orig_esc="$(fc -ln -1)"
  orig_esc="${orig_esc## }"
  orig_esc="${orig_esc//;/\\;}"
  orig_esc="${orig_esc//$'\n'/\\n}"
  printf '\033]133;B;%s;cwd:%s;orig:%s\007' "$cmd_esc" "$pwd_esc" "$orig_esc"
  printf '\033]133;C\007'
}

# ZLE widget for readline reporting (bound to same key as bash)
__omnish-rl-report() {
  printf '\033]133;RL;%s;%s\007' "$BUFFER" "$CURSOR"
}
zle -N __omnish-rl-report
bindkey '\e[13337~' __omnish-rl-report

# Append to hook arrays (coexists with oh-my-zsh/prezto/p10k)
precmd_functions+=(__omnish_precmd)
preexec_functions+=(__omnish_preexec)
"#;
```

- [ ] **Step 2: Add unit test for ZSH_HOOK content**

Add to the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn test_zsh_hook_content_has_osc133_sequences() {
        assert!(ZSH_HOOK.contains("133;A"));
        assert!(ZSH_HOOK.contains("133;B"));
        assert!(ZSH_HOOK.contains("133;C"));
        assert!(ZSH_HOOK.contains("133;D"));
        assert!(ZSH_HOOK.contains("133;RL"));
    }
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p omnish-client --release -- shell_hook`
Expected: all `shell_hook` tests pass including the new one.

- [ ] **Step 4: Commit**

```bash
git add crates/omnish-client/src/shell_hook.rs
git commit -m "feat: add ZSH_HOOK constant with OSC 133 support (#462)"
```

---

### Task 2: Add install_zsh_hook() function

**Files:**
- Modify: `crates/omnish-client/src/shell_hook.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing test module:

```rust
    #[test]
    fn test_zsh_returns_zdotdir() {
        let result = install_zsh_hook("/bin/zsh");
        assert!(result.is_some());
        let zdotdir = result.unwrap();
        assert!(zdotdir.exists());
        assert!(zdotdir.is_dir());
        // Must contain a .zshrc that sources the hook
        let zshrc = zdotdir.join(".zshrc");
        assert!(zshrc.exists());
        let content = std::fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains("zsh_hook.zsh"), "zshrc should source hook: {content}");
    }

    #[test]
    fn test_zsh_hook_preserves_original_zdotdir() {
        let result = install_zsh_hook("/usr/bin/zsh");
        assert!(result.is_some());
        let zdotdir = result.unwrap();
        let zshrc = zdotdir.join(".zshrc");
        let content = std::fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains("OMNISH_ORIG_ZDOTDIR"), "should reference original ZDOTDIR: {content}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p omnish-client --release -- shell_hook`
Expected: FAIL — `install_zsh_hook` not found.

- [ ] **Step 3: Implement install_zsh_hook()**

Add after `install_bash_hook()` in `shell_hook.rs`:

```rust
/// Install the zsh OSC 133 hook.
/// Returns the ZDOTDIR path containing a `.zshrc` that sources the hook,
/// or None if the shell is not zsh.
pub fn install_zsh_hook(shell: &str) -> Option<PathBuf> {
    if !shell.ends_with("zsh") {
        return None;
    }

    let dir = omnish_common::config::omnish_dir().join("hooks");
    std::fs::create_dir_all(&dir).ok()?;

    // Write the hook script (only if content differs or file doesn't exist)
    let hook_path = dir.join("zsh_hook.zsh");
    let should_write = match std::fs::read(&hook_path) {
        Ok(existing) => existing != ZSH_HOOK.as_bytes(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };
    if should_write {
        std::fs::write(&hook_path, ZSH_HOOK).ok()?;
    }

    // Create a ZDOTDIR with a .zshrc that sources user config then our hook.
    // Zsh doesn't have --rcfile; ZDOTDIR is the standard override mechanism.
    let zdotdir = dir.join("zdotdir");
    std::fs::create_dir_all(&zdotdir).ok()?;

    let zshrc_path = zdotdir.join(".zshrc");
    let mut content = String::new();
    // Source user's original .zshrc from their real home or original ZDOTDIR
    content.push_str(
        "# Source user's .zshrc from original ZDOTDIR (or HOME)\n\
         if [[ -f \"${OMNISH_ORIG_ZDOTDIR:-$HOME}/.zshrc\" ]]; then\n\
         \tsource \"${OMNISH_ORIG_ZDOTDIR:-$HOME}/.zshrc\"\n\
         fi\n"
    );
    content.push_str(&format!(
        "source \"{}\"\n",
        hook_path.to_string_lossy()
    ));

    let should_write_rc = match std::fs::read(&zshrc_path) {
        Ok(existing) => existing != content.as_bytes(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    };
    if should_write_rc {
        std::fs::write(&zshrc_path, &content).ok()?;
    }

    Some(zdotdir)
}
```

- [ ] **Step 4: Update test_non_bash_returns_none to reflect new behavior**

The existing test `test_non_bash_returns_none` asserts that zsh returns None from `install_bash_hook`. This is still correct — `install_bash_hook("/bin/zsh")` should return None. No change needed.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p omnish-client --release -- shell_hook`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/omnish-client/src/shell_hook.rs
git commit -m "feat: add install_zsh_hook() with ZDOTDIR mechanism (#462)"
```

---

### Task 3: Integrate zsh hook into shell spawning

**Files:**
- Modify: `crates/omnish-client/src/main.rs`

- [ ] **Step 1: Refactor hook installation to support both shells**

In `main.rs` around line 487-494, replace the current bash-only hook code:

```rust
    // Resolve shell and hook args early so they're available for respawn.
    let shell = resolve_shell(&config.shell.command);
    let osc133_rcfile = shell_hook::install_bash_hook(&shell);
    let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
        vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
    } else {
        vec![]
    };
    let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();
```

With:

```rust
    // Resolve shell and hook args early so they're available for respawn.
    let shell = resolve_shell(&config.shell.command);

    // Install shell-specific OSC 133 hook
    let osc133_rcfile = shell_hook::install_bash_hook(&shell);
    let osc133_zdotdir = shell_hook::install_zsh_hook(&shell);
    let osc133_hook_installed = osc133_rcfile.is_some() || osc133_zdotdir.is_some();

    let shell_args: Vec<String> = if let Some(ref rcfile) = osc133_rcfile {
        vec!["--rcfile".to_string(), rcfile.to_string_lossy().to_string()]
    } else {
        vec![]
    };
    let shell_args_ref: Vec<&str> = shell_args.iter().map(|s| s.as_str()).collect();
```

- [ ] **Step 2: Pass ZDOTDIR as environment variable for zsh**

In the normal startup block (around line 508), update `child_env` to include ZDOTDIR for zsh:

```rust
        let mut child_env = HashMap::new();
        child_env.insert("OMNISH_SESSION_ID".to_string(), session_id.clone());
        child_env.insert("SHELL".to_string(), shell.clone());
        if let Some(ref zdotdir) = osc133_zdotdir {
            // Preserve original ZDOTDIR so the hook can source the user's .zshrc
            if let Ok(orig) = std::env::var("ZDOTDIR") {
                child_env.insert("OMNISH_ORIG_ZDOTDIR".to_string(), orig);
            }
            child_env.insert("ZDOTDIR".to_string(), zdotdir.to_string_lossy().to_string());
        }
```

- [ ] **Step 3: Fix osc133_hook_installed usage**

The old code derived `osc133_hook_installed` from `osc133_rcfile.is_some()` in two places (normal startup line 512, resume line 503). Replace both with the new `osc133_hook_installed` variable computed above. The resume path (line 503) should use the same variable:

Change line 503 from:
```rust
        (resume.session_id.clone(), proxy, osc133_rcfile.is_some())
```
To:
```rust
        (resume.session_id.clone(), proxy, osc133_hook_installed)
```

And remove the now-redundant line 512:
```rust
        let osc133_hook_installed = osc133_rcfile.is_some();
```

Since `osc133_hook_installed` is already computed above.

- [ ] **Step 4: Build to verify compilation**

Run: `cargo build --release -p omnish-client`
Expected: compiles without errors or warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/omnish-client/src/main.rs
git commit -m "feat: integrate zsh hook installation and ZDOTDIR into shell spawning (#462)"
```

---

### Task 4: Parameterize integration test framework for multi-shell

**Files:**
- Modify: `tools/integration_tests/lib.sh`

- [ ] **Step 1: Add TEST_SHELL support to lib.sh**

Replace the hardcoded tmux default-shell line (line 35):

```bash
echo "set -g default-shell /bin/bash" > "$TMUX_CONF"
```

With:

```bash
# ── Shell selection (TEST_SHELL env var, default: bash) ─────────────────
TEST_SHELL="${TEST_SHELL:-bash}"
_resolve_test_shell() {
    case "$TEST_SHELL" in
        bash) echo "/bin/bash" ;;
        zsh)  echo "/bin/zsh" ;;
        *)    echo "/bin/$TEST_SHELL" ;;
    esac
}
TEST_SHELL_PATH="$(_resolve_test_shell)"
if [[ ! -x "$TEST_SHELL_PATH" ]]; then
    echo -e "${YELLOW}SKIP: $TEST_SHELL_PATH not found${NC}"
    exit 0
fi
echo "set -g default-shell $TEST_SHELL_PATH" > "$TMUX_CONF"
```

- [ ] **Step 2: Update is_shell_prompt to handle zsh**

Replace the existing `is_shell_prompt` function (lines 331-335):

```bash
is_shell_prompt() {
    local last
    last=$(last_nonempty_line "$1")
    echo "$last" | grep -qE '[\$#] $|\$$|\#$'
}
```

With:

```bash
# is_shell_prompt <content>
#   Returns 0 if the last non-empty line looks like a shell prompt.
#   Matches bash ($ or #) and zsh (% or #) prompt endings.
is_shell_prompt() {
    local last
    last=$(last_nonempty_line "$1")
    echo "$last" | grep -qE '[\$#%] $|[\$#%]$'
}
```

- [ ] **Step 3: Add shell info to test output**

In the `test_init` function, after the dependency check (line 89), add a line to print the shell being tested:

```bash
    echo -e "${YELLOW}Shell: $TEST_SHELL ($TEST_SHELL_PATH)${NC}"
```

- [ ] **Step 4: Test the change manually**

Run: `TEST_SHELL=bash bash tools/integration_tests/test_basic.sh -t 1`
Expected: test 1 passes, output shows "Shell: bash (/bin/bash)".

If zsh is installed, also run:
`TEST_SHELL=zsh bash tools/integration_tests/test_basic.sh -t 1`
Expected: test 1 passes (or at least starts and shows "Shell: zsh (/bin/zsh)").

- [ ] **Step 5: Commit**

```bash
git add tools/integration_tests/lib.sh
git commit -m "feat: parameterize integration tests with TEST_SHELL env var (#462)"
```

---

### Task 5: Add zsh CI job

**Files:**
- Modify: `.gitlab-ci.yml`

- [ ] **Step 1: Add integration-test-zsh job**

Add after the existing `integration-test` job (after line 82):

```yaml
integration-test-zsh:
  stage: test
  image: docker.nv/ubuntu_2404_runner:latest
  script:
    - apt-get update -qq && apt-get install -y -qq tmux zsh locales >/dev/null 2>&1
    - locale-gen en_US.UTF-8
    - export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
    - cargo build --release
    # Setup omnish environment
    - mkdir -p ~/.omnish
    - echo "$DAEMON_TOML" > ~/.omnish/daemon.toml
    - mkdir -p ~/.omnish/bin
    - cp target/release/omnish-plugin ~/.omnish/bin/
    - cp -r plugins/ ~/.omnish/plugins/
    - ./target/release/omnish-daemon --init
    # Start daemon in background and wait for socket
    - export OMNISH_SOCKET="$HOME/.omnish/omnish.sock"
    - ./target/release/omnish-daemon &
    - |
      for i in $(seq 1 10); do
        [ -S "$OMNISH_SOCKET" ] && break
        sleep 1
      done
    - bash tools/integration_tests/prepare_threads.sh
    # Run integration tests under zsh
    - export TEST_SHELL=zsh
    - bash tools/integration_tests/test_basic.sh
    - bash tools/integration_tests/test_tool_display.sh
    - bash tools/integration_tests/test_spinner.sh
    - bash tools/integration_tests/verify_issue_127.sh
    - bash tools/integration_tests/verify_issue_144.sh
    - bash tools/integration_tests/verify_issue_147.sh
    - bash tools/integration_tests/verify_issue_149.sh
    - bash tools/integration_tests/verify_issue_286.sh
    - bash tools/integration_tests/verify_issue_288.sh
    - bash tools/integration_tests/verify_issue_354.sh
    - bash tools/integration_tests/verify_resume_shortcut.sh
    - bash tools/integration_tests/verify_scroll_view.sh
    - bash tools/integration_tests/test_menu.sh
    - bash tools/integration_tests/verify_issue_180.sh
    - bash tools/integration_tests/verify_issue_184.sh
    - bash tools/integration_tests/verify_issue_337.sh
    - bash tools/integration_tests/verify_issue_342.sh
    - bash tools/integration_tests/verify_picker_selection.sh
    - bash tools/integration_tests/test_config_push.sh
    - bash tools/integration_tests/test_sandbox_rules.sh
    - bash tools/integration_tests/test_config_backend.sh
  after_script:
    - kill $(pgrep -f omnish-daemon) 2>/dev/null || true
  rules:
    - if: $CI_PIPELINE_SOURCE == "schedule"
```

- [ ] **Step 2: Verify YAML is valid**

Run: `python3 -c "import yaml; yaml.safe_load(open('.gitlab-ci.yml'))"`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add .gitlab-ci.yml
git commit -m "ci: add integration-test-zsh job (#462)"
```

---

### Task 6: Manual end-to-end validation

This task is manual — run omnish under zsh locally and verify the hook works.

- [ ] **Step 1: Build**

Run: `cargo build --release`

- [ ] **Step 2: Test omnish under zsh**

Ask user to start `omnish-daemon`, then run:
```bash
SHELL=/bin/zsh ./target/release/omnish
```

Verify:
1. Zsh prompt appears normally
2. Run a command (e.g. `ls`) — check that omnish tracks it (`:` → `/debug session` should show command count > 0)
3. Enter chat mode with `:` — verify it works
4. Ghost text completions appear (requires daemon with LLM configured)

- [ ] **Step 3: Run integration tests under both shells**

```bash
TEST_SHELL=bash bash tools/integration_tests/test_basic.sh
TEST_SHELL=zsh bash tools/integration_tests/test_basic.sh
```

Fix any test failures discovered under zsh. Common issues to expect:
- Prompt detection timing differences (zsh may be faster/slower to display prompt)
- Different default prompt format (zsh uses `%` not `$`)
- Bash-specific test commands (e.g. `bind`) that don't exist in zsh

- [ ] **Step 4: Commit any test fixes**

```bash
git add -u
git commit -m "fix: adjust integration tests for zsh compatibility (#462)"
```
