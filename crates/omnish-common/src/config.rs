use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Serde helpers that accept both integer and string representations for numeric
/// config fields, e.g. `context_window = 200000` and `context_window = "200000"`.
mod string_or_int {
    use serde::{self, Deserialize, Deserializer};
    use std::fmt;
    use std::marker::PhantomData;
    use std::str::FromStr;

    struct NumVisitor<T>(PhantomData<T>);

    impl<'de, T> serde::de::Visitor<'de> for NumVisitor<T>
    where
        T: Deserialize<'de> + FromStr,
        T::Err: fmt::Display,
    {
        type Value = T;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an integer or a string containing an integer")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<T, E> {
            let s = v.to_string();
            T::from_str(&s).map_err(E::custom)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<T, E> {
            let s = v.to_string();
            T::from_str(&s).map_err(E::custom)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<T, E> {
            T::from_str(v).map_err(E::custom)
        }
    }

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + FromStr,
        T::Err: fmt::Display,
    {
        deserializer.deserialize_any(NumVisitor(PhantomData))
    }

    pub mod option {
        use super::*;

        struct OptNumVisitor<T>(PhantomData<T>);

        impl<'de, T> serde::de::Visitor<'de> for OptNumVisitor<T>
        where
            T: Deserialize<'de> + FromStr,
            T::Err: fmt::Display,
        {
            type Value = Option<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("null, an integer, or a string containing an integer")
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Option<T>, E> {
                Ok(None)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Option<T>, E> {
                Ok(None)
            }
            fn visit_some<D2: Deserializer<'de>>(self, d: D2) -> Result<Option<T>, D2::Error> {
                super::deserialize(d).map(Some)
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Option<T>, E> {
                NumVisitor(PhantomData).visit_i64(v).map(Some)
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Option<T>, E> {
                NumVisitor(PhantomData).visit_u64(v).map(Some)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Option<T>, E> {
                NumVisitor(PhantomData).visit_str(v).map(Some)
            }
        }

        pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
        where
            D: Deserializer<'de>,
            T: Deserialize<'de> + FromStr,
            T::Err: fmt::Display,
        {
            deserializer.deserialize_any(OptNumVisitor(PhantomData))
        }
    }
}

/// Serde helper that accepts both `true`/`false` and `"true"`/`"false"` for bool fields.
mod string_or_bool {
    use serde::{self, Deserializer};

    struct BoolVisitor;

    impl<'de> serde::de::Visitor<'de> for BoolVisitor {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a boolean or a string containing a boolean")
        }
        fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<bool, E> {
            Ok(v)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<bool, E> {
            v.parse().map_err(E::custom)
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoolVisitor)
    }
}

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

#[derive(Debug, Deserialize, Clone)]
pub struct ClientConfig {
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default = "default_socket_path")]
    pub daemon_addr: String,
    #[serde(default)]
    pub onboarded: bool,
    #[serde(default)]
    pub sandbox: ClientSandboxConfig,
}

/// Client-local sandbox settings. Per-host because sandbox capability
/// depends on kernel/OS features (bwrap, landlock, seatbelt).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ClientSandboxConfig {
    /// Master on/off switch for all client-side sandbox usage.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Preferred backend: "bwrap" | "landlock" | "macos".
    /// Client-side availability detection may override this at runtime.
    #[serde(default = "default_client_sandbox_backend")]
    pub backend: String,
    /// Per-tool permit rules (host-local). Merged with daemon-side rules
    /// at runtime; these take precedence for local exemptions.
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}

fn default_client_sandbox_backend() -> String {
    if cfg!(target_os = "macos") {
        "macos".to_string()
    } else {
        "bwrap".to_string()
    }
}

impl Default for ClientSandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: default_client_sandbox_backend(),
            plugins: HashMap::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            shell: ShellConfig::default(),
            daemon_addr: default_socket_path(),
            onboarded: false,
            sandbox: ClientSandboxConfig::default(),
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
// ConfigMap: generic key-value config (used by tasks and plugins)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ConfigMap {
    values: HashMap<String, serde_json::Value>,
    defaults: HashMap<String, serde_json::Value>,
}

impl ConfigMap {
    pub fn set_defaults(&mut self, defaults: HashMap<String, serde_json::Value>) {
        self.defaults = defaults;
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.values.get(key)
            .or_else(|| self.defaults.get(key))
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    pub fn get_u64(&self, key: &str, default: u64) -> u64 {
        match self.values.get(key).or_else(|| self.defaults.get(key)) {
            Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(default),
            Some(serde_json::Value::String(s)) => s.parse().unwrap_or(default),
            _ => default,
        }
    }

    pub fn get_string(&self, key: &str, default: &str) -> String {
        self.values.get(key)
            .or_else(|| self.defaults.get(key))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| default.to_string())
    }

    pub fn get_opt_string(&self, key: &str) -> Option<String> {
        self.values.get(key)
            .or_else(|| self.defaults.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.values.contains_key(key) || self.defaults.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.values.get(key).or_else(|| self.defaults.get(key))
    }

    /// Iterate over merged view (values override defaults).
    pub fn iter(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> + '_ {
        self.defaults.iter()
            .filter(|(k, _)| !self.values.contains_key(*k))
            .chain(self.values.iter())
    }
}

/// Compare user-set values only (defaults don't affect config diff).
impl PartialEq for ConfigMap {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

/// Serialize: output merged values + defaults (for menu display via toml::Value).
/// File writes go through config_edit, not serialization.
impl serde::Serialize for ConfigMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut merged = self.defaults.clone();
        merged.extend(self.values.iter().map(|(k, v)| (k.clone(), v.clone())));
        merged.serialize(serializer)
    }
}

/// Deserialize: only populate values, defaults remain empty.
impl<'de> serde::Deserialize<'de> for ConfigMap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = HashMap::<String, serde_json::Value>::deserialize(deserializer)?;
        Ok(Self { values, defaults: HashMap::new() })
    }
}

impl From<HashMap<String, serde_json::Value>> for ConfigMap {
    fn from(map: HashMap<String, serde_json::Value>) -> Self {
        Self { values: map, defaults: HashMap::new() }
    }
}

// ---------------------------------------------------------------------------
// TasksConfig: per-task key-value config
// ---------------------------------------------------------------------------

pub type TasksConfig = HashMap<String, ConfigMap>;

// ---------------------------------------------------------------------------
// Sandbox config
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct SandboxConfig {
    /// Per-tool permit rules. Key is tool_name (e.g. "bash").
    /// When any rule matches, the tool runs without Landlock sandbox.
    ///
    /// Note: The sandbox backend selection and on/off switch live on the
    /// client side (`ClientConfig.sandbox`) because sandbox capability is
    /// host-specific. This struct only holds daemon-wide permit rules.
    #[serde(default)]
    pub plugins: HashMap<String, SandboxPluginConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxPluginConfig {
    /// Rules in format: "<param_field> <operator> <value>"
    /// Operators: starts_with, contains, equals, matches (regex)
    #[serde(default)]
    pub permit_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct ProxyConfig {
    #[serde(default)]
    pub http_proxy: Option<String>,
    #[serde(default)]
    pub no_proxy: Option<String>,
}

/// Custom deserializer: accepts both `proxy = "http://..."` (old) and `[proxy]` table (new).
impl<'de> serde::Deserialize<'de> for ProxyConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ProxyVisitor;
        impl<'de> serde::de::Visitor<'de> for ProxyVisitor {
            type Value = ProxyConfig;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a proxy URL string or a [proxy] table")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ProxyConfig, E> {
                Ok(ProxyConfig {
                    http_proxy: Some(v.to_string()),
                    no_proxy: None,
                })
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<ProxyConfig, M::Error> {
                #[derive(Deserialize)]
                struct Inner {
                    #[serde(default)]
                    http_proxy: Option<String>,
                    #[serde(default)]
                    no_proxy: Option<String>,
                }
                let inner = Inner::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(ProxyConfig {
                    http_proxy: inner.http_proxy,
                    no_proxy: inner.no_proxy,
                })
            }
        }
        deserializer.deserialize_any(ProxyVisitor)
    }
}

// ---------------------------------------------------------------------------
// Daemon config
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub listen_addr: String,
    #[serde(default)]
    pub proxy: ProxyConfig,
    /// Backward compat: old top-level `no_proxy` key merges into `proxy.no_proxy`.
    #[serde(default, skip_serializing)]
    no_proxy: Option<String>,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub tasks: TasksConfig,
    /// Per-plugin configuration.
    /// [plugins.web_search]
    /// enabled = false
    /// api_key = "..."
    #[serde(default)]
    pub plugins: HashMap<String, ConfigMap>,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub client: ClientSection,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_socket_path(),
            proxy: ProxyConfig::default(),
            no_proxy: None,
            llm: LlmConfig::default(),
            context: ContextConfig::default(),
            tasks: HashMap::new(),
            plugins: HashMap::new(),
            sandbox: SandboxConfig::default(),
            client: ClientSection::default(),
        }
    }
}

impl DaemonConfig {
    /// Merge deprecated top-level `no_proxy` into `proxy.no_proxy`.
    pub fn normalize(&mut self) {
        if let Some(np) = self.no_proxy.take() {
            if self.proxy.no_proxy.is_none() {
                self.proxy.no_proxy = Some(np);
            }
        }
    }
}

pub fn load_daemon_config() -> Result<DaemonConfig> {
    let path = std::env::var("OMNISH_DAEMON_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| omnish_dir().join("daemon.toml"));
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        let mut config: DaemonConfig = match toml::from_str(&contents) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "WARNING: failed to parse {}: {}; attempting to sanitize",
                    path.display(),
                    e
                );
                toml::from_str(&sanitize_toml(&contents))?
            }
        };
        config.normalize();
        Ok(config)
    } else {
        Ok(DaemonConfig::default())
    }
}

/// Sanitize TOML text by removing duplicate table headers and duplicate keys
/// within each section. Keeps the first occurrence of each.
fn sanitize_toml(input: &str) -> String {
    use std::collections::HashSet;
    let mut seen_tables = HashSet::new();
    // Keys seen in the current section (reset on each new table header)
    let mut seen_keys = HashSet::<String>::new();
    let mut output = String::with_capacity(input.len());
    let mut skip_table = false;

    for line in input.lines() {
        let trimmed = line.trim();
        // Detect table headers like [foo] or [foo.bar] (but not array-of-tables [[foo]])
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            if let Some(end) = trimmed.find(']') {
                let table = &trimmed[1..end];
                if !seen_tables.insert(table.to_string()) {
                    skip_table = true;
                    continue;
                } else {
                    skip_table = false;
                    seen_keys.clear();
                }
            }
        } else if skip_table {
            continue;
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            // Check for duplicate key within current section
            if let Some(key) = trimmed.split('=').next() {
                let key = key.trim();
                if !key.is_empty() && !seen_keys.insert(key.to_string()) {
                    continue;
                }
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
pub struct ShellConfig {
    #[serde(default = "default_shell_command")]
    pub command: String,
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
    /// Prefix to resume last chat thread (default: "::")
    #[serde(default = "default_resume_prefix")]
    pub resume_prefix: String,
    #[serde(default = "default_intercept_gap_ms", deserialize_with = "string_or_int::deserialize")]
    pub intercept_gap_ms: u64,
    #[serde(default = "default_ghost_timeout_ms", deserialize_with = "string_or_int::deserialize")]
    pub ghost_timeout_ms: u64,
    /// When true, prevents : and :: from triggering chat mode when command line already has content
    #[serde(default = "default_developer_mode", deserialize_with = "string_or_bool::deserialize")]
    pub developer_mode: bool,
    #[serde(default = "default_true", deserialize_with = "string_or_bool::deserialize")]
    pub completion_enabled: bool,
    /// Use extended Unicode characters (e.g. ⎿) in the UI.
    /// Set to false for terminals lacking font support (e.g. ConEmu with default fonts).
    /// In the future this may be set automatically via terminal detection.
    #[serde(default = "default_true", deserialize_with = "string_or_bool::deserialize")]
    pub extended_unicode: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: default_shell_command(),
            command_prefix: default_command_prefix(),
            resume_prefix: default_resume_prefix(),
            intercept_gap_ms: default_intercept_gap_ms(),
            ghost_timeout_ms: default_ghost_timeout_ms(),
            developer_mode: default_developer_mode(),
            completion_enabled: true,
            extended_unicode: true,
        }
    }
}

/// Daemon-owned client settings pushed to connected clients.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ClientSection {
    #[serde(default = "default_command_prefix")]
    pub command_prefix: String,
    #[serde(default = "default_resume_prefix")]
    pub resume_prefix: String,
    #[serde(default = "default_true", deserialize_with = "string_or_bool::deserialize")]
    pub completion_enabled: bool,
    #[serde(default = "default_ghost_timeout_ms", deserialize_with = "string_or_int::deserialize")]
    pub ghost_timeout_ms: u64,
    #[serde(default = "default_intercept_gap_ms", deserialize_with = "string_or_int::deserialize")]
    pub intercept_gap_ms: u64,
    #[serde(default = "default_developer_mode", deserialize_with = "string_or_bool::deserialize")]
    pub developer_mode: bool,
}

impl Default for ClientSection {
    fn default() -> Self {
        Self {
            command_prefix: default_command_prefix(),
            resume_prefix: default_resume_prefix(),
            completion_enabled: true,
            ghost_timeout_ms: default_ghost_timeout_ms(),
            intercept_gap_ms: default_intercept_gap_ms(),
            developer_mode: default_developer_mode(),
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

fn default_developer_mode() -> bool {
    false
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
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
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LangfuseConfig {
    #[serde(default)]
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

fn default_backend_type() -> String {
    "openai-compat".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LlmBackendConfig {
    #[serde(default = "default_backend_type")]
    pub backend_type: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key_cmd: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Whether to use the global proxy for this backend (default: false)
    #[serde(default)]
    pub use_proxy: bool,
    /// Context window size in tokens (model-specific).
    /// When max_content_chars is not set, defaults to context_window * 1.5.
    #[serde(default, deserialize_with = "string_or_int::option::deserialize")]
    pub context_window: Option<usize>,
    /// Maximum content characters for context. Advanced override.
    /// If not set, derived from context_window * 1.5.
    #[serde(default, deserialize_with = "string_or_int::option::deserialize")]
    pub max_content_chars: Option<usize>,
}

// ---------------------------------------------------------------------------
// Context config
// ---------------------------------------------------------------------------

/// Completion-specific context configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompletionContextConfig {
    /// Number of recent commands shown with full detail (output, timing, exit code).
    #[serde(default = "default_detailed_commands", deserialize_with = "string_or_int::deserialize")]
    pub detailed_commands: usize,
    /// Number of older commands listed as command-line only (no output).
    #[serde(default = "default_history_commands", deserialize_with = "string_or_int::deserialize")]
    pub history_commands: usize,
    #[serde(default = "default_head_lines", deserialize_with = "string_or_int::deserialize")]
    pub head_lines: usize,
    #[serde(default = "default_tail_lines", deserialize_with = "string_or_int::deserialize")]
    pub tail_lines: usize,
    /// Maximum width (in characters) per output line; longer lines are truncated.
    #[serde(default = "default_max_line_width", deserialize_with = "string_or_int::deserialize")]
    pub max_line_width: usize,
    /// Minimum number of commands to keep from the current session.
    #[serde(default = "default_min_current_session_commands", deserialize_with = "string_or_int::deserialize")]
    pub min_current_session_commands: usize,
    /// Maximum character limit for completion context.
    /// If exceeded, the system will try reducing history_commands + detailed_commands by 1/4.
    #[serde(default = "default_max_context_chars", deserialize_with = "string_or_int::option::deserialize")]
    pub max_context_chars: Option<usize>,
    /// Minimum number of detailed commands after elastic window reset.
    #[serde(default = "default_detailed_min", deserialize_with = "string_or_int::deserialize")]
    pub detailed_min: usize,
    /// Maximum number of detailed commands before elastic window reset.
    #[serde(default = "default_detailed_max", deserialize_with = "string_or_int::deserialize")]
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

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ContextConfig {
    #[serde(default)]
    pub completion: CompletionContextConfig,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_config_serializes_to_toml() {
        let config = DaemonConfig::default();
        let value = toml::Value::try_from(&config).unwrap();
        assert!(value.get("llm").is_some());
        assert!(value.get("llm").unwrap().get("backends").is_some());
        assert!(value.get("proxy").is_some());
        assert!(value.get("proxy").unwrap().is_table());
    }

    #[test]
    fn test_sanitize_toml_duplicate_tables() {
        let input = r#"
listen_addr = "/tmp/omnish.sock"

[tasks.auto_update]
enabled = true

[tasks.eviction]
session_evict_hours = 48

[tasks.auto_update]
enabled = false
schedule = "0 0 4 * * *"
"#;
        let output = sanitize_toml(input);
        assert_eq!(output.matches("[tasks.auto_update]").count(), 1);
        assert!(output.contains("enabled = true"));
        assert!(!output.contains("enabled = false"));
        assert!(output.contains("[tasks.eviction]"));
        assert!(output.contains("session_evict_hours = 48"));
        let config: DaemonConfig = toml::from_str(&output).unwrap();
        assert!(config.tasks["auto_update"].get_bool("enabled", false));
    }

    #[test]
    fn test_sanitize_toml_duplicate_keys() {
        let input = r#"
listen_addr = "/tmp/omnish.sock"
listen_addr = "/tmp/other.sock"

[tasks.auto_update]
enabled = true
enabled = false
schedule = "0 0 4 * * *"
"#;
        let output = sanitize_toml(input);
        assert_eq!(output.matches("listen_addr").count(), 1);
        assert!(output.contains(r#"listen_addr = "/tmp/omnish.sock""#));
        assert!(!output.contains("other.sock"));
        assert_eq!(output.matches("enabled").count(), 1);
        assert!(output.contains("enabled = true"));
        assert!(output.contains("schedule"));
        let config: DaemonConfig = toml::from_str(&output).unwrap();
        assert_eq!(config.listen_addr, "/tmp/omnish.sock");
        assert!(config.tasks["auto_update"].get_bool("enabled", false));
    }
}
