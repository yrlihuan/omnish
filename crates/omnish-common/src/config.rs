use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Returns the omnish base directory: `~/.omnish`, fallback `/tmp/omnish`.
pub fn omnish_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".omnish"))
        .unwrap_or_else(|| PathBuf::from("/tmp/omnish"))
}

fn default_socket_path() -> String {
    omnish_dir()
        .join("omnish.sock")
        .to_string_lossy()
        .to_string()
}

fn default_sessions_dir() -> String {
    omnish_dir()
        .join("sessions")
        .to_string_lossy()
        .to_string()
}

// ---------------------------------------------------------------------------
// Client config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default = "default_socket_path")]
    pub daemon_addr: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig::default(),
            daemon_addr: default_socket_path(),
        }
    }
}

pub fn load_client_config() -> Result<ClientConfig> {
    let path = std::env::var("OMNISH_CLIENT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| omnish_dir().join("client.toml"));
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&contents)?)
    } else {
        Ok(ClientConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Daemon config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub listen_addr: String,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default = "default_sessions_dir")]
    pub sessions_dir: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_socket_path(),
            llm: LlmConfig::default(),
            sessions_dir: default_sessions_dir(),
        }
    }
}

pub fn load_daemon_config() -> Result<DaemonConfig> {
    let path = std::env::var("OMNISH_DAEMON_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| omnish_dir().join("daemon.toml"));
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&contents)?)
    } else {
        Ok(DaemonConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "default_shell_command")]
    pub command: String,
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
    #[serde(default = "default_intercept_gap_ms")]
    pub intercept_gap_ms: u64,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: default_shell_command(),
            command_prefix: default_command_prefix(),
            intercept_gap_ms: default_intercept_gap_ms(),
        }
    }
}

fn default_shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

fn default_command_prefix() -> String {
    ":".to_string()
}

fn default_intercept_gap_ms() -> u64 {
    1000
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_name")]
    pub default: String,
    #[serde(default)]
    pub backends: HashMap<String, LlmBackendConfig>,
    #[serde(default)]
    pub auto_trigger: AutoTriggerConfig,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default: default_llm_name(),
            backends: HashMap::new(),
            auto_trigger: AutoTriggerConfig::default(),
        }
    }
}

fn default_llm_name() -> String {
    "claude".to_string()
}

#[derive(Debug, Deserialize)]
pub struct LlmBackendConfig {
    pub backend_type: String,
    pub model: String,
    #[serde(default)]
    pub api_key_cmd: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AutoTriggerConfig {
    #[serde(default)]
    pub on_nonzero_exit: bool,
    #[serde(default)]
    pub on_stderr_patterns: Vec<String>,
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
}

fn default_cooldown() -> u64 {
    5
}
