use omnish_common::config::OmnishConfig;

#[test]
fn test_parse_default_config() {
    let toml_str = r#"
[shell]
command = "/bin/bash"
command_prefix = "::"

[daemon]
socket_path = "/tmp/omnish.sock"

[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "echo test-key"

[llm.auto_trigger]
on_nonzero_exit = true
on_stderr_patterns = ["error", "panic", "traceback", "fatal"]
cooldown_seconds = 5
"#;
    let config: OmnishConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.shell.command, "/bin/bash");
    assert_eq!(config.shell.command_prefix, "::");
    assert_eq!(config.llm.default, "claude");
    assert!(config.llm.auto_trigger.on_nonzero_exit);
    assert_eq!(config.llm.auto_trigger.cooldown_seconds, 5);
}

#[test]
fn test_config_defaults() {
    let toml_str = r#"
[llm]
default = "claude"

[llm.backends.claude]
backend_type = "anthropic"
model = "claude-sonnet-4-5-20250929"
api_key_cmd = "echo key"
"#;
    let config: OmnishConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.shell.command_prefix, "::");
    assert!(!config.llm.auto_trigger.on_nonzero_exit);
}
