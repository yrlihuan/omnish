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

"#;
    let config: DaemonConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.listen_addr, "/tmp/omnish.sock");
    assert_eq!(config.llm.default, "claude");
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
fn test_load_daemon_config_duplicate_table_recovers() {
    let path = "/tmp/omnish-test-dup-section.toml";
    std::fs::write(
        path,
        r#"
listen_addr = "/tmp/test.sock"

[tasks.auto_update]
enabled = true

[tasks.auto_update]
enabled = false
"#,
    )
    .unwrap();
    std::env::set_var("OMNISH_DAEMON_CONFIG", path);
    let config = omnish_common::config::load_daemon_config().unwrap();
    assert_eq!(config.listen_addr, "/tmp/test.sock");
    assert!(config.tasks["auto_update"].get_bool("enabled", false));
    std::env::remove_var("OMNISH_DAEMON_CONFIG");
    std::fs::remove_file(path).ok();
}

#[test]
fn test_omnish_dir() {
    let dir = omnish_common::config::omnish_dir();
    assert!(dir.to_string_lossy().ends_with(".omnish"));
}

#[test]
fn test_int_fields_accept_string_values() {
    // Integer fields should accept both native TOML integers and quoted strings
    let toml_str = r#"
[llm.backends.test]
backend_type = "openai-compat"
model = "gpt-4o"
context_window = "128000"
max_content_chars = "192000"

[context.completion]
detailed_commands = "10"
history_commands = "100"
head_lines = "5"
tail_lines = "5"
max_line_width = "80"
max_context_chars = "50000"
detailed_min = "2"
detailed_max = "20"

[tasks.eviction]
session_evict_hours = "24"

[tasks.daily_notes]
"#;
    let config: DaemonConfig = toml::from_str(toml_str).unwrap();
    let backend = &config.llm.backends["test"];
    assert_eq!(backend.context_window, Some(128000));
    assert_eq!(backend.max_content_chars, Some(192000));
    assert_eq!(config.context.completion.detailed_commands, 10);
    assert_eq!(config.context.completion.history_commands, 100);
    assert_eq!(config.context.completion.max_context_chars, Some(50000));
    assert_eq!(
        config.tasks["eviction"].get_u64("session_evict_hours", 48),
        24
    );

    // ShellConfig int fields (in ClientConfig)
    let client_toml = r#"
[shell]
intercept_gap_ms = "500"
ghost_timeout_ms = "5000"
"#;
    let client: ClientConfig = toml::from_str(client_toml).unwrap();
    assert_eq!(client.shell.intercept_gap_ms, 500);
    assert_eq!(client.shell.ghost_timeout_ms, 5000);
}

#[test]
fn test_int_fields_still_accept_native_integers() {
    let toml_str = r#"
[llm.backends.test]
backend_type = "openai-compat"
model = "gpt-4o"
context_window = 128000
"#;
    let config: DaemonConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.llm.backends["test"].context_window, Some(128000));
}
