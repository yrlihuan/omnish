use std::path::PathBuf;

const BASH_HOOK: &str = r#"
# omnish shell integration — OSC 133 semantic prompts
__omnish_prompt_cmd() {
  local ec=$?
  printf '\033]133;D;%d\007' "$ec"
  printf '\033]133;A\007'
}
PROMPT_COMMAND="__omnish_prompt_cmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

__omnish_preexec() {
  if [[ "$BASH_COMMAND" != "$PROMPT_COMMAND" ]] && [[ "$BASH_COMMAND" != __omnish_* ]]; then
    printf '\033]133;B\007'
    printf '\033]133;C\007'
  fi
}
trap '__omnish_preexec' DEBUG
"#;

/// Write the bash hook script and return the path.
/// Returns None if the shell is not bash.
pub fn install_bash_hook(shell: &str) -> Option<PathBuf> {
    if !shell.ends_with("bash") {
        return None;
    }

    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("omnish");
    std::fs::create_dir_all(&dir).ok()?;

    let path = dir.join("bash_hook.sh");
    std::fs::write(&path, BASH_HOOK).ok()?;
    Some(path)
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
    fn test_bash_returns_path() {
        let result = install_bash_hook("/bin/bash");
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("__omnish_prompt_cmd"));
    }
}
