use std::path::{Path, PathBuf};

/// Atomic write: tmp file in same directory + rename. Avoids the truncate
/// window in std::fs::write where a concurrent reader can observe an empty
/// or partially written file.
fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir")
    })?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{}.tmp.{}.{}", name, std::process::id(), nanos));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

const BASH_HOOK: &str = r#"
# omnish shell integration - OSC 133 semantic prompts
__omnish_preexec_fired=0
__omnish_in_precmd=0

__omnish_prompt_cmd() {
  __omnish_in_precmd=0
  __omnish_preexec_fired=0
  printf '\033]133;D;%d\007' "$__omnish_last_ec"
  printf '\033]133;A\007'
}
# Bracket PROMPT_COMMAND:
#   1. __omnish_last_ec=$? - capture exit code (must be first; assignments reset $?)
#      Also sets in_precmd guard in same compound assignment to avoid extra $? reset.
#   2. <user's PROMPT_COMMAND entries>
#   3. __omnish_prompt_cmd - emit OSC 133 D+A, reset flags
# Strip trailing semicolons/whitespace to avoid ";;" syntax errors.
__omnish_pc="$PROMPT_COMMAND"
while [[ "$__omnish_pc" =~ [[:space:]\;]$ ]]; do __omnish_pc="${__omnish_pc%?}"; done
PROMPT_COMMAND="__omnish_last_ec=\$? __omnish_in_precmd=1;${__omnish_pc:+$__omnish_pc;}type __omnish_prompt_cmd &>/dev/null && __omnish_prompt_cmd"
unset __omnish_pc

__omnish_preexec() {
  [[ "$__omnish_in_precmd" == "1" ]] && return
  [[ "$__omnish_preexec_fired" == "1" ]] && return
  [[ "$BASH_COMMAND" == __omnish_* ]] && return
  __omnish_preexec_fired=1
  # Escape semicolons in command and PWD for OSC 133 payload
  local cmd_esc="${BASH_COMMAND//;/\\;}"
  local pwd_esc="${PWD//;/\\;}"
  # Extract original user input from history (preserves aliases unexpanded)
  local orig_esc
  orig_esc="$(HISTTIMEFORMAT= history 1 | sed 's/^[ ]*[0-9]*[ ]*//')"
  orig_esc="${orig_esc//;/\\;}"
  printf '\033]133;B;%s;cwd:%s;orig:%s\007' "$cmd_esc" "$pwd_esc" "$orig_esc"
  printf '\033]133;C\007'
}

__omnish_rl_report() {
    printf '\033]133;RL;%s;%s\007' "$READLINE_LINE" "$READLINE_POINT"
}
# bind -x requires bash 4.0+ with readline support
if bind -x '"\e[13337~": __omnish_rl_report' 2>/dev/null; then
  # Only bind in emacs-isearch if the keymap exists (not available on all bash versions)
  bind -m emacs-isearch &>/dev/null && bind -m emacs-isearch -x '"\e[13337~": __omnish_rl_report'
else
  printf '\033]133;NO_READLINE\007'
fi

# DEBUG trap set last so hook init commands (bind etc.) are not recorded (#395)
trap '__omnish_preexec' DEBUG
"#;

const ZSH_HOOK: &str = r#"
# omnish shell integration - OSC 133 semantic prompts for zsh
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

/// Build the `source` lines for a bash login shell's profile chain.
///
/// A real interactive login bash reads `/etc/profile` then the first existing
/// of `~/.bash_profile`, `~/.bash_login`, `~/.profile`. omnish spawns bash as a
/// non-login shell (so `--rcfile`, and thus the OSC 133 hook, keeps working), so
/// when omnish itself was started as a login shell we replay that profile chain
/// here to restore PATH / env / ssh-agent setup (issue #649). Empty when no
/// profile file exists.
fn bash_login_profile_sources() -> String {
    let mut out = String::new();
    let etc_profile = PathBuf::from("/etc/profile");
    if etc_profile.exists() {
        out.push_str(&format!("source \"{}\"\n", etc_profile.to_string_lossy()));
    }
    if let Some(home) = dirs::home_dir() {
        // Bash stops at the first existing file in this order.
        for name in &[".bash_profile", ".bash_login", ".profile"] {
            let p = home.join(name);
            if p.exists() {
                out.push_str(&format!("source \"{}\"\n", p.to_string_lossy()));
                break;
            }
        }
    }
    out
}

/// Generate an rcfile that sources the user's original bashrc then loads the OSC 133 hook.
/// When `is_login` is true (omnish started as a login shell), the login profile
/// chain (`/etc/profile`, `~/.bash_profile` chain) is sourced first so login
/// initialization is not lost.
/// Returns the rcfile path, or None if the shell is not bash.
pub fn install_bash_hook(shell: &str, is_login: bool) -> Option<PathBuf> {
    if !shell.ends_with("bash") {
        return None;
    }

    let dir = omnish_common::config::omnish_dir().join("hooks");
    std::fs::create_dir_all(&dir).ok()?;

    // Write the hook script, but only if content differs or file doesn't exist
    let hook_path = dir.join("bash_hook.sh");
    let should_write = match std::fs::read(&hook_path) {
        Ok(existing) => existing != BASH_HOOK.as_bytes(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false, // other error (e.g., permission) - skip writing
    };
    if should_write {
        write_atomic(&hook_path, BASH_HOOK.as_bytes()).ok()?;
    }

    // Write an rcfile that (for login shells) replays the login profile chain,
    // then sources the user's bashrc, then the hook.
    // Use a login-specific filename so a login and a non-login session on the
    // same host do not overwrite each other's rcfile.
    let rcfile_path = dir.join(if is_login { "bashrc.login" } else { "bashrc" });
    let bashrc = dirs::home_dir()
        .map(|h| h.join(".bashrc"))
        .filter(|p| p.exists());
    let mut content = String::new();
    if is_login {
        content.push_str(&bash_login_profile_sources());
    }
    if let Some(ref bashrc) = bashrc {
        content.push_str(&format!(
            "source \"{}\"\n",
            bashrc.to_string_lossy()
        ));
    }
    content.push_str(&format!(
        "source \"{}\"\n",
        hook_path.to_string_lossy()
    ));
    let should_write_rc = match std::fs::read(&rcfile_path) {
        Ok(existing) => existing != content.as_bytes(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false, // other error - skip writing
    };
    if should_write_rc {
        write_atomic(&rcfile_path, content.as_bytes()).ok()?;
    }

    Some(rcfile_path)
}

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
        write_atomic(&hook_path, ZSH_HOOK.as_bytes()).ok()?;
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
        write_atomic(&zshrc_path, content.as_bytes()).ok()?;
    }

    Some(zdotdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_content_has_osc133_sequences() {
        assert!(BASH_HOOK.contains("133;A"));
        assert!(BASH_HOOK.contains("133;B"));
        assert!(BASH_HOOK.contains("133;C"));
        assert!(BASH_HOOK.contains("133;D"));
    }

    #[test]
    fn test_non_bash_returns_none() {
        assert!(install_bash_hook("/bin/zsh", false).is_none());
        assert!(install_bash_hook("/bin/fish", false).is_none());
    }

    #[test]
    fn test_bash_returns_rcfile_path() {
        let result = install_bash_hook("/bin/bash", false);
        assert!(result.is_some());
        let rcfile = result.unwrap();
        assert!(rcfile.exists());
        assert!(rcfile.to_string_lossy().ends_with("bashrc"));
        let content = std::fs::read_to_string(&rcfile).unwrap();
        // rcfile sources the hook script
        assert!(content.contains("bash_hook.sh"), "rcfile should source hook: {content}");
    }

    #[test]
    fn test_login_uses_separate_rcfile_and_sources_profile() {
        let non_login = install_bash_hook("/bin/bash", false).unwrap();
        let login = install_bash_hook("/bin/bash", true).unwrap();

        // Login and non-login rcfiles must not clobber each other.
        assert_ne!(non_login, login);
        assert!(login.to_string_lossy().ends_with("bashrc.login"));

        let content = std::fs::read_to_string(&login).unwrap();
        // Still sources the OSC 133 hook.
        assert!(content.contains("bash_hook.sh"), "login rcfile should source hook: {content}");
        // Sources at least one login profile file when one exists on the host.
        let has_profile = std::path::Path::new("/etc/profile").exists()
            || dirs::home_dir().is_some_and(|h| {
                [".bash_profile", ".bash_login", ".profile"]
                    .iter()
                    .any(|n| h.join(n).exists())
            });
        if has_profile {
            assert!(
                content.contains("/etc/profile")
                    || content.contains(".bash_profile")
                    || content.contains(".bash_login")
                    || content.contains(".profile"),
                "login rcfile should source a login profile: {content}"
            );
        }

        // Non-login rcfile must NOT pull in the profile chain.
        let non_login_content = std::fs::read_to_string(&non_login).unwrap();
        assert!(!non_login_content.contains("/etc/profile"), "non-login rcfile must not source /etc/profile: {non_login_content}");
    }

    #[test]
    fn test_hook_content_includes_cwd() {
        assert!(BASH_HOOK.contains("PWD"));
        assert!(BASH_HOOK.contains("cwd:"));
    }

    #[test]
    fn test_zsh_hook_content_has_osc133_sequences() {
        assert!(ZSH_HOOK.contains("133;A"));
        assert!(ZSH_HOOK.contains("133;B"));
        assert!(ZSH_HOOK.contains("133;C"));
        assert!(ZSH_HOOK.contains("133;D"));
        assert!(ZSH_HOOK.contains("133;RL"));
    }

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
}
