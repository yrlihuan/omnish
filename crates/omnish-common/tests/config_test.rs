use omnish_common::config::{ClientConfig, DaemonConfig};

#[test]
fn test_parse_client_config() {
    let toml_str = r#"
daemon_addr = "/tmp/omnish.sock"

[shell]
command = "/bin/bash"
command_prefix = ":"
"#;
    let config: ClientConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.shell.command, "/bin/bash");
    assert_eq!(config.shell.command_prefix, ":");
    assert_eq!(config.daemon_addr, "/tmp/omnish.sock");
}

#[test]
fn test_parse_daemon_config() {
    let toml_str = r#"
listen_addr = "/tmp/omnish.sock"

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
    let config: DaemonConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.listen_addr, "/tmp/omnish.sock");
    assert_eq!(config.llm.default, "claude");
    assert!(config.llm.auto_trigger.on_nonzero_exit);
    assert_eq!(config.llm.auto_trigger.cooldown_seconds, 5);
}

#[test]
fn test_client_config_defaults() {
    let toml_str = "";
    let config: ClientConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.shell.command_prefix, ":");
    assert!(config.daemon_addr.ends_with("omnish.sock"));
}

#[test]
fn test_daemon_config_defaults() {
    let toml_str = "";
    let config: DaemonConfig = toml::from_str(toml_str).unwrap();
    assert!(config.listen_addr.ends_with("omnish.sock"));
    assert_eq!(config.llm.default, "claude");
    assert!(!config.llm.auto_trigger.on_nonzero_exit);
}

#[test]
fn test_load_client_config_missing_file_returns_default() {
    std::env::set_var("OMNISH_CLIENT_CONFIG", "/tmp/nonexistent-omnish-test-client.toml");
    let config = omnish_common::config::load_client_config().unwrap();
    assert_eq!(config.shell.command_prefix, ":");
    std::env::remove_var("OMNISH_CLIENT_CONFIG");
}

#[test]
fn test_load_daemon_config_missing_file_returns_default() {
    std::env::set_var("OMNISH_DAEMON_CONFIG", "/tmp/nonexistent-omnish-test-daemon.toml");
    let config = omnish_common::config::load_daemon_config().unwrap();
    assert_eq!(config.llm.default, "claude");
    std::env::remove_var("OMNISH_DAEMON_CONFIG");
}

#[test]
fn test_omnish_dir() {
    let dir = omnish_common::config::omnish_dir();
    assert!(dir.to_string_lossy().ends_with(".omnish"));
}
