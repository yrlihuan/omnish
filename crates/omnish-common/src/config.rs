use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Returns the omnish base directory.
/// Priority: `$OMNISH_HOME` > `~/.omnish` > `/tmp/omnish`.
pub fn omnish_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OMNISH_HOME") {
        return PathBuf::from(dir);
    }
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

// ---------------------------------------------------------------------------
// Client config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default = "default_socket_path")]
    pub daemon_addr: String,
    #[serde(default = "default_true")]
    pub completion_enabled: bool,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default)]
    pub onboarded: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig::default(),
            daemon_addr: default_socket_path(),
            completion_enabled: true,
            auto_update: false,
            onboarded: false,
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

fn default_disk_cleanup_schedule() -> String {
    "0 0 */6 * * *".to_string()
}

// ---------------------------------------------------------------------------
// Tasks config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
pub struct EvictionConfig {
    /// Evict sessions from memory after this many hours of inactivity.
    #[serde(default = "default_session_evict_hours")]
    pub session_evict_hours: u64,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            session_evict_hours: default_session_evict_hours(),
        }
    }
}

fn default_session_evict_hours() -> u64 {
    48
}

#[derive(Debug, Deserialize, Clone)]
pub struct DiskCleanupConfig {
    #[serde(default = "default_disk_cleanup_schedule")]
    pub schedule: String,
}

impl Default for DiskCleanupConfig {
    fn default() -> Self {
        Self {
            schedule: default_disk_cleanup_schedule(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AutoUpdateConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Cron schedule for update checks. Default: daily at 04:00.
    #[serde(default = "default_auto_update_schedule")]
    pub schedule: String,
    /// Client hosts to distribute updates to (e.g. ["user@host1", "user@host2"]).
    #[serde(default)]
    pub clients: Vec<String>,
    /// Update source: local directory path or GitHub API URL.
    /// Local path: "/data/releases" — scans for latest tar.gz
    /// GitHub URL: "https://api.github.com/repos/user/repo/releases/latest"
    /// If omitted, defaults to GitHub (yrlihuan/omnish).
    #[serde(default)]
    pub check_url: Option<String>,
}

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: default_auto_update_schedule(),
            clients: Vec::new(),
            check_url: None,
        }
    }
}

fn default_auto_update_schedule() -> String {
    "0 0 4 * * *".to_string()
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct TasksConfig {
    #[serde(default)]
    pub eviction: EvictionConfig,
    #[serde(default)]
    pub daily_notes: DailyNotesConfig,
    #[serde(default)]
    pub disk_cleanup: DiskCleanupConfig,
    #[serde(default)]
    pub auto_update: AutoUpdateConfig,
}

// ---------------------------------------------------------------------------
// Plugins config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone, Default)]
pub struct PluginsConfig {
    /// List of enabled plugin names. Each corresponds to a directory
    /// under ~/.omnish/plugins/{name}/ containing a {name} executable.
    #[serde(default)]
    pub enabled: Vec<String>,
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
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub tasks: TasksConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_socket_path(),
            llm: LlmConfig::default(),
            context: ContextConfig::default(),
            tasks: TasksConfig::default(),
            plugins: PluginsConfig::default(),
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
    /// Prefix to resume last chat thread (default: "::")
    #[serde(default = "default_resume_prefix")]
    pub resume_prefix: String,
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
            resume_prefix: default_resume_prefix(),
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

fn default_resume_prefix() -> String {
    "::".to_string()
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
    /// Map use cases to backend names
    /// Example:
    ///   [llm.use_cases]
    ///   completion = "claude-fast"
    ///   analysis = "claude"
    ///   chat = "claude"
    #[serde(default)]
    pub use_cases: HashMap<String, String>,
    /// Optional Langfuse observability integration
    #[serde(default)]
    pub langfuse: Option<LangfuseConfig>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default: default_llm_name(),
            backends: HashMap::new(),
            use_cases: HashMap::new(),
            langfuse: None,
        }
    }
}

/// Langfuse observability configuration.
///
/// Example:
///   [llm.langfuse]
///   public_key = "pk-..."
///   secret_key = "sk-lf-..."
///   base_url = "https://cloud.langfuse.com"
#[derive(Debug, Deserialize)]
pub struct LangfuseConfig {
    pub public_key: String,
    #[serde(default)]
    pub secret_key: Option<String>,
    #[serde(default = "default_langfuse_base_url")]
    pub base_url: String,
}

fn default_langfuse_base_url() -> String {
    "https://cloud.langfuse.com".to_string()
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
    /// Maximum content characters for context (model-specific limit)
    #[serde(default)]
    pub max_content_chars: Option<usize>,
}

// ---------------------------------------------------------------------------
// Context config
// ---------------------------------------------------------------------------

/// Completion-specific context configuration
#[derive(Debug, Deserialize, Clone)]
pub struct CompletionContextConfig {
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
    /// Minimum number of commands to keep from the current session.
    #[serde(default = "default_min_current_session_commands")]
    pub min_current_session_commands: usize,
    /// Maximum character limit for completion context.
    /// If exceeded, the system will try reducing history_commands + detailed_commands by 1/4.
    #[serde(default = "default_max_context_chars")]
    pub max_context_chars: Option<usize>,
    /// Minimum number of detailed commands after elastic window reset.
    #[serde(default = "default_detailed_min")]
    pub detailed_min: usize,
    /// Maximum number of detailed commands before elastic window reset.
    #[serde(default = "default_detailed_max")]
    pub detailed_max: usize,
}

impl Default for CompletionContextConfig {
    fn default() -> Self {
        Self {
            detailed_commands: default_detailed_commands(),
            history_commands: default_history_commands(),
            head_lines: default_head_lines(),
            tail_lines: default_tail_lines(),
            max_line_width: default_max_line_width(),
            min_current_session_commands: default_min_current_session_commands(),
            max_context_chars: default_max_context_chars(),
            detailed_min: default_detailed_min(),
            detailed_max: default_detailed_max(),
        }
    }
}

// ---------------------------------------------------------------------------
// Hourly summary config
// ---------------------------------------------------------------------------

/// Hourly summary context configuration
#[derive(Debug, Deserialize, Clone)]
pub struct HourlySummaryConfig {
    /// Number of lines to keep from the start of each command output.
    #[serde(default = "default_hourly_head_lines")]
    pub head_lines: usize,
    /// Number of lines to keep from the end of each command output.
    #[serde(default = "default_hourly_tail_lines")]
    pub tail_lines: usize,
    /// Maximum width (in characters) per output line; longer lines are truncated.
    #[serde(default = "default_hourly_max_line_width")]
    pub max_line_width: usize,
}

impl Default for HourlySummaryConfig {
    fn default() -> Self {
        Self {
            head_lines: default_hourly_head_lines(),
            tail_lines: default_hourly_tail_lines(),
            max_line_width: default_hourly_max_line_width(),
        }
    }
}

fn default_hourly_head_lines() -> usize {
    50
}

fn default_hourly_tail_lines() -> usize {
    100
}

fn default_hourly_max_line_width() -> usize {
    128
}

// ---------------------------------------------------------------------------
// Daily summary config
// ---------------------------------------------------------------------------

/// Daily summary context configuration
#[derive(Debug, Deserialize, Clone)]
pub struct DailySummaryConfig {
    /// Number of lines to keep from the start of each command output.
    #[serde(default = "default_daily_head_lines")]
    pub head_lines: usize,
    /// Number of lines to keep from the end of each command output.
    #[serde(default = "default_daily_tail_lines")]
    pub tail_lines: usize,
    /// Maximum width (in characters) per output line; longer lines are truncated.
    #[serde(default = "default_daily_max_line_width")]
    pub max_line_width: usize,
}

impl Default for DailySummaryConfig {
    fn default() -> Self {
        Self {
            head_lines: default_daily_head_lines(),
            tail_lines: default_daily_tail_lines(),
            max_line_width: default_daily_max_line_width(),
        }
    }
}

fn default_daily_head_lines() -> usize {
    20
}

fn default_daily_tail_lines() -> usize {
    50
}

fn default_daily_max_line_width() -> usize {
    128
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ContextConfig {
    #[serde(default)]
    pub completion: CompletionContextConfig,
    #[serde(default)]
    pub hourly_summary: HourlySummaryConfig,
    #[serde(default)]
    pub daily_summary: DailySummaryConfig,
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
    200
}

fn default_min_current_session_commands() -> usize {
    5
}

fn default_max_context_chars() -> Option<usize> {
    None
}

fn default_detailed_min() -> usize {
    20
}

fn default_detailed_max() -> usize {
    30
}
