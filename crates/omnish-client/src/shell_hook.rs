use std::path::PathBuf;

const BASH_HOOK: &str = r#"
# omnish shell integration — OSC 133 semantic prompts
__omnish_preexec_fired=0
__omnish_in_precmd=0

__omnish_prompt_cmd() {
  __omnish_in_precmd=0
  __omnish_preexec_fired=0
  printf '\033]133;D;%d\007' "$__omnish_last_ec"
  printf '\033]133;A\007'
}
# Bracket PROMPT_COMMAND:
#   1. __omnish_last_ec=$? — capture exit code (must be first; assignments reset $?)
#      Also sets in_precmd guard in same compound assignment to avoid extra $? reset.
#   2. <user's PROMPT_COMMAND entries>
#   3. __omnish_prompt_cmd — emit OSC 133 D+A, reset flags
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
  printf '\033]133;B;%s;cwd:%s\007' "$cmd_esc" "$pwd_esc"
  printf '\033]133;C\007'
}
trap '__omnish_preexec' DEBUG

__omnish_rl_report() {
    printf '\033]133;RL;%s\007' "$READLINE_LINE"
}
bind -x '"\e[13337~": __omnish_rl_report'
"#;

/// Generate an rcfile that sources the user's original bashrc then loads the OSC 133 hook.
/// Returns the rcfile path, or None if the shell is not bash.
pub fn install_bash_hook(shell: &str) -> Option<PathBuf> {
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
        Err(_) => false, // other error (e.g., permission) — skip writing
    };
    if should_write {
        std::fs::write(&hook_path, BASH_HOOK).ok()?;
    }

    // Write an rcfile that sources the user's bashrc first, then the hook
    let rcfile_path = dir.join("bashrc");
    let bashrc = dirs::home_dir()
        .map(|h| h.join(".bashrc"))
        .filter(|p| p.exists());
    let mut content = String::new();
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
        Err(_) => false, // other error — skip writing
    };
    if should_write_rc {
        std::fs::write(&rcfile_path, &content).ok()?;
    }

    Some(rcfile_path)
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
        assert!(install_bash_hook("/bin/zsh").is_none());
        assert!(install_bash_hook("/bin/fish").is_none());
    }

    #[test]
    fn test_bash_returns_rcfile_path() {
        let result = install_bash_hook("/bin/bash");
        assert!(result.is_some());
        let rcfile = result.unwrap();
        assert!(rcfile.exists());
        assert!(rcfile.to_string_lossy().ends_with("bashrc"));
        let content = std::fs::read_to_string(&rcfile).unwrap();
        // rcfile sources the hook script
        assert!(content.contains("bash_hook.sh"), "rcfile should source hook: {content}");
    }

    #[test]
    fn test_hook_content_includes_cwd() {
        assert!(BASH_HOOK.contains("PWD"));
        assert!(BASH_HOOK.contains("cwd:"));
    }
}
