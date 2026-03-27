use crate::anthropic::AnthropicBackend;
use crate::backend::{BackendInfo, LlmBackend, UseCase};
use crate::langfuse::{LangfuseBackend, LangfuseConfig};
use crate::openai_compat::OpenAiCompatBackend;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use omnish_common::config::{LlmBackendConfig, LlmConfig};
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::sync::RwLock;

/// Resolve API key from command or direct value
fn resolve_api_key(api_key_cmd: &Option<String>) -> Result<String> {
    match api_key_cmd {
        Some(cmd) => {
            let output = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .map_err(|e| anyhow!("failed to execute api_key_cmd: {}", e))?;

            if !output.status.success() {
                return Err(anyhow!(
                    "api_key_cmd failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            let key = String::from_utf8(output.stdout)?
                .trim()
                .to_string();
            if key.is_empty() {
                return Err(anyhow!("api_key_cmd returned empty key"));
            }
            Ok(key)
        }
        None => Err(anyhow!("no api_key_cmd specified")),
    }
}

/// Build a reqwest client with optional proxy support.
fn build_http_client(proxy: Option<&str>, no_proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .pool_max_idle_per_host(10);
    if let Some(proxy_url) = proxy {
        let mut p = reqwest::Proxy::all(proxy_url)?;
        if let Some(no_proxy_str) = no_proxy {
            p = p.no_proxy(reqwest::NoProxy::from_string(no_proxy_str));
        }
        builder = builder.proxy(p);
    }
    Ok(builder.build()?)
}

/// Create LLM backend from config
pub fn create_backend(
    name: &str,
    config: &LlmBackendConfig,
    proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> Result<Arc<dyn LlmBackend>> {
    let api_key = resolve_api_key(&config.api_key_cmd)?;

    match config.backend_type.as_str() {
        "anthropic" => {
            let client = build_http_client(proxy, no_proxy)?;
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com".to_string());
            Ok(Arc::new(AnthropicBackend {
                config_name: name.to_string(),
                api_key,
                model: config.model.clone(),
                base_url,
                client,
            }))
        }
        "openai" | "openai-compat" => {
            let base_url = config
                .base_url
                .clone()
                .ok_or_else(|| anyhow!("openai-compat requires base_url"))?;
            let client = build_http_client(proxy, no_proxy)?;
            Ok(Arc::new(OpenAiCompatBackend {
                config_name: name.to_string(),
                api_key,
                model: config.model.clone(),
                base_url,
                client,
            }))
        }
        other => Err(anyhow!("unknown backend type: {}", other)),
    }
}

/// Create default LLM backend from config
pub fn create_default_backend(llm_config: &LlmConfig, proxy: Option<&str>, no_proxy: Option<&str>) -> Result<Arc<dyn LlmBackend>> {
    let backend_name = &llm_config.default;
    let backend_config = llm_config
        .backends
        .get(backend_name)
        .ok_or_else(|| anyhow!("default backend '{}' not found in config", backend_name))?;

    create_backend(backend_name, backend_config, proxy, no_proxy)
}

/// MultiBackend routes LLM requests to different backends based on use case
pub struct MultiBackend {
    /// Map from use case name to backend
    use_case_backends: RwLock<HashMap<String, Arc<dyn LlmBackend>>>,
    /// Default backend for unknown use cases
    default_backend: Arc<dyn LlmBackend>,
    /// Map from use case name to max_content_chars
    use_case_max_chars: HashMap<String, Option<usize>>,
    /// All backends by config name (for per-thread model selection).
    named_backends: HashMap<String, Arc<dyn LlmBackend>>,
    /// Backend info list for listing available models.
    backend_configs: Vec<BackendInfo>,
    /// Default chat backend name.
    chat_backend_name: String,
}

impl MultiBackend {
    /// Create a MultiBackend from LLM config
    pub fn new(llm_config: &LlmConfig, proxy: Option<&str>, no_proxy: Option<&str>) -> Result<Self> {
        // Resolve Langfuse config if present
        let langfuse_config = resolve_langfuse_config(llm_config, proxy, no_proxy);

        // First pass: create all backends by config name
        let mut named_backends = HashMap::new();
        let mut backend_configs = Vec::new();
        for (name, cfg) in &llm_config.backends {
            match create_backend(name, cfg, proxy, no_proxy) {
                Ok(backend) => {
                    let backend = maybe_wrap_langfuse(backend, &langfuse_config);
                    named_backends.insert(name.clone(), backend);
                    backend_configs.push(BackendInfo {
                        name: name.clone(),
                        model: cfg.model.clone(),
                    });
                }
                Err(e) => {
                    tracing::warn!("backend '{}' failed to initialize: {}", name, e);
                }
            }
        }
        backend_configs.sort_by(|a, b| a.name.cmp(&b.name));

        // Second pass: map use cases to backends
        let use_case_backends = RwLock::new(HashMap::new());
        let mut use_case_max_chars = HashMap::new();
        for (use_case_name, backend_name) in &llm_config.use_cases {
            if let Some(backend) = named_backends.get(backend_name) {
                use_case_backends
                    .write()
                    .map_err(|_| anyhow!("failed to acquire write lock"))?
                    .insert(use_case_name.clone(), backend.clone());
                if let Some(cfg) = llm_config.backends.get(backend_name) {
                    use_case_max_chars.insert(use_case_name.clone(), cfg.max_content_chars);
                }
            } else {
                tracing::warn!(
                    "backend '{}' not available for use case '{}'",
                    backend_name, use_case_name
                );
            }
        }

        // Resolve default: configured default > first working backend > error
        let default_backend = named_backends.get(&llm_config.default)
            .cloned()
            .or_else(|| named_backends.values().next().cloned())
            .ok_or_else(|| {
                anyhow!("no LLM backends could be initialized — check backend_type values in daemon.toml")
            })?;

        let chat_backend_name = llm_config.use_cases
            .get("chat")
            .cloned()
            .unwrap_or_else(|| llm_config.default.clone());

        Ok(Self {
            use_case_backends,
            default_backend,
            use_case_max_chars,
            named_backends,
            backend_configs,
            chat_backend_name,
        })
    }

    /// Get backend for the given use case
    pub fn get_backend(&self, use_case: UseCase) -> Arc<dyn LlmBackend> {
        let use_case_name = match use_case {
            UseCase::Completion => "completion",
            UseCase::Analysis => "analysis",
            UseCase::Chat => "chat",
        };

        self.use_case_backends
            .read()
            .ok()
            .and_then(|backends| backends.get(use_case_name).cloned())
            .unwrap_or_else(|| self.default_backend.clone())
    }

    /// Get max_content_chars for the given use case
    pub fn get_max_content_chars(&self, use_case: UseCase) -> Option<usize> {
        let use_case_name = match use_case {
            UseCase::Completion => "completion",
            UseCase::Analysis => "analysis",
            UseCase::Chat => "chat",
        };

        self.use_case_max_chars
            .get(use_case_name)
            .copied()
            .flatten()
    }
}

#[async_trait]
impl LlmBackend for MultiBackend {
    async fn complete(&self, req: &crate::backend::LlmRequest) -> Result<crate::backend::LlmResponse> {
        let backend = self.get_backend(req.use_case);
        backend.complete(req).await
    }

    fn name(&self) -> &str {
        "multi"
    }

    fn max_content_chars_for_use_case(&self, use_case: crate::backend::UseCase) -> Option<usize> {
        self.get_max_content_chars(use_case)
    }

    fn list_backends(&self) -> Vec<BackendInfo> {
        self.backend_configs.clone()
    }

    fn chat_default_name(&self) -> &str {
        &self.chat_backend_name
    }

    fn get_backend_by_name(&self, name: &str) -> Option<Arc<dyn LlmBackend>> {
        self.named_backends.get(name).cloned()
    }
}

/// Resolve Langfuse configuration, returning None if not configured or key missing.
fn resolve_langfuse_config(llm_config: &LlmConfig, proxy: Option<&str>, no_proxy: Option<&str>) -> Option<LangfuseConfig> {
    let cfg = llm_config.langfuse.as_ref()?;
    let secret_key = match &cfg.secret_key {
        Some(key) if !key.is_empty() => key.clone(),
        _ => {
            tracing::warn!("langfuse secret_key not set, disabling langfuse");
            return None;
        }
    };
    tracing::info!("langfuse enabled: {}", cfg.base_url);
    Some(LangfuseConfig {
        public_key: cfg.public_key.clone(),
        secret_key,
        host: cfg.base_url.clone(),
        proxy: proxy.map(|s| s.to_string()),
        no_proxy: no_proxy.map(|s| s.to_string()),
    })
}

/// Wrap a backend with Langfuse tracing if config is present.
fn maybe_wrap_langfuse(
    backend: Arc<dyn LlmBackend>,
    langfuse_config: &Option<LangfuseConfig>,
) -> Arc<dyn LlmBackend> {
    match langfuse_config {
        Some(cfg) => LangfuseBackend::wrap(backend, cfg.clone()),
        None => backend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnish_common::config::LlmBackendConfig;

    #[test]
    fn test_resolve_api_key_with_echo() {
        let cmd = Some("echo test-key-123".to_string());
        let key = resolve_api_key(&cmd).unwrap();
        assert_eq!(key, "test-key-123");
    }

    #[test]
    fn test_resolve_api_key_missing() {
        let result = resolve_api_key(&None);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_anthropic_backend() {
        let config = LlmBackendConfig {
            backend_type: "anthropic".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            api_key_cmd: Some("echo sk-test-key".to_string()),
            base_url: None,
            max_content_chars: None,
        };

        let backend = create_backend("test", &config, None, None).unwrap();
        assert_eq!(backend.name(), "test");
    }

    #[test]
    fn test_create_openai_compat_backend() {
        let config = LlmBackendConfig {
            backend_type: "openai-compat".to_string(),
            model: "gpt-4".to_string(),
            api_key_cmd: Some("echo sk-test-key".to_string()),
            base_url: Some("https://api.openai.com/v1".to_string()),
            max_content_chars: None,
        };

        let backend = create_backend("test", &config, None, None).unwrap();
        assert_eq!(backend.name(), "test");
    }

    #[test]
    fn test_create_openai_compat_without_base_url_fails() {
        let config = LlmBackendConfig {
            backend_type: "openai-compat".to_string(),
            model: "gpt-4".to_string(),
            api_key_cmd: Some("echo sk-test-key".to_string()),
            base_url: None,
            max_content_chars: None,
        };

        let result = create_backend("test", &config, None, None);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("base_url"));
    }

    #[test]
    fn test_unknown_backend_type() {
        let config = LlmBackendConfig {
            backend_type: "unknown".to_string(),
            model: "model".to_string(),
            api_key_cmd: Some("echo key".to_string()),
            base_url: None,
            max_content_chars: None,
        };

        let result = create_backend("test", &config, None, None);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("unknown backend"));
    }
}
