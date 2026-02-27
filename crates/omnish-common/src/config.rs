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
// Daily notes config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
pub struct DailyNotesConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_schedule_hour")]
    pub schedule_hour: u8,
}

impl Default for DailyNotesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule_hour: default_schedule_hour(),
        }
    }
}

fn default_schedule_hour() -> u8 {
    23
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
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub daily_notes: DailyNotesConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_socket_path(),
            llm: LlmConfig::default(),
            sessions_dir: default_sessions_dir(),
            context: ContextConfig::default(),
            daily_notes: DailyNotesConfig::default(),
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
    #[serde(default = "default_ghost_timeout_ms")]
    pub ghost_timeout_ms: u64,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: default_shell_command(),
            command_prefix: default_command_prefix(),
            intercept_gap_ms: default_intercept_gap_ms(),
            ghost_timeout_ms: default_ghost_timeout_ms(),
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

fn default_ghost_timeout_ms() -> u64 {
    10_000
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_name")]
    pub default: String,
    #[serde(default)]
    pub backends: HashMap<String, LlmBackendConfig>,
    #[serde(default)]
    pub auto_trigger: AutoTriggerConfig,
    /// Map use cases to backend names
    /// Example:
    ///   [llm.use_cases]
    ///   completion = "claude-fast"
    ///   analysis = "claude"
    ///   chat = "claude"
    #[serde(default)]
    pub use_cases: HashMap<String, String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default: default_llm_name(),
            backends: HashMap::new(),
            auto_trigger: AutoTriggerConfig::default(),
            use_cases: HashMap::new(),
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

// ---------------------------------------------------------------------------
// Context config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
pub struct ContextConfig {
    /// Number of recent commands shown with full detail (output, timing, exit code).
    #[serde(default = "default_detailed_commands")]
    pub detailed_commands: usize,
    /// Number of older commands listed as command-line only (no output).
    #[serde(default = "default_history_commands")]
    pub history_commands: usize,
    #[serde(default = "default_head_lines")]
    pub head_lines: usize,
    #[serde(default = "default_tail_lines")]
    pub tail_lines: usize,
    /// Maximum width (in characters) per output line; longer lines are truncated.
    #[serde(default = "default_max_line_width")]
    pub max_line_width: usize,
    /// Evict sessions from memory after this many hours of inactivity.
    #[serde(default = "default_session_evict_hours")]
    pub session_evict_hours: u64,
    /// Minimum number of commands to keep from the current session.
    #[serde(default = "default_min_current_session_commands")]
    pub min_current_session_commands: usize,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            detailed_commands: default_detailed_commands(),
            history_commands: default_history_commands(),
            head_lines: default_head_lines(),
            tail_lines: default_tail_lines(),
            max_line_width: default_max_line_width(),
            session_evict_hours: default_session_evict_hours(),
            min_current_session_commands: default_min_current_session_commands(),
        }
    }
}

fn default_detailed_commands() -> usize {
    30
}

fn default_history_commands() -> usize {
    500
}

fn default_head_lines() -> usize {
    20
}

fn default_tail_lines() -> usize {
    20
}

fn default_max_line_width() -> usize {
    512
}

fn default_session_evict_hours() -> u64 {
    48
}

fn default_min_current_session_commands() -> usize {
    5
}
