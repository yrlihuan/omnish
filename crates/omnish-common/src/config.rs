use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct OmnishConfig {
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub llm: LlmConfig,
}

impl Default for OmnishConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig::default(),
            daemon: DaemonConfig::default(),
            llm: LlmConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "default_shell_command")]
    pub command: String,
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: default_shell_command(),
            command_prefix: default_command_prefix(),
        }
    }
}

fn default_shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

fn default_command_prefix() -> String {
    "::".to_string()
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub socket_path: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
        }
    }
}

fn default_socket_path() -> String {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{}/omnish.sock", runtime_dir)
    } else {
        "/tmp/omnish.sock".to_string()
    }
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

fn default_llm_name() -> String {
    "claude".to_string()
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

pub fn load_config() -> Result<OmnishConfig> {
    let path = std::env::var("OMNISH_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/etc"))
                .join("omnish/config.toml")
        });
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&contents)?)
    } else {
        Ok(OmnishConfig::default())
    }
}
