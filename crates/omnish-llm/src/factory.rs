use crate::anthropic::AnthropicBackend;
use crate::backend::{LlmBackend, UseCase};
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

/// Create LLM backend from config
pub fn create_backend(
    _name: &str,
    config: &LlmBackendConfig,
) -> Result<Arc<dyn LlmBackend>> {
    let api_key = resolve_api_key(&config.api_key_cmd)?;

    match config.backend_type.as_str() {
        "anthropic" => {
            let client = reqwest::Client::builder()
                .pool_max_idle_per_host(10)
                .build()?;
            Ok(Arc::new(AnthropicBackend {
                api_key,
                model: config.model.clone(),
                client,
            }))
        }
        "openai-compat" => {
            let base_url = config
                .base_url
                .clone()
                .ok_or_else(|| anyhow!("openai-compat requires base_url"))?;
            let client = reqwest::Client::builder()
                .pool_max_idle_per_host(10)
                .build()?;
            Ok(Arc::new(OpenAiCompatBackend {
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
pub fn create_default_backend(llm_config: &LlmConfig) -> Result<Arc<dyn LlmBackend>> {
    let backend_name = &llm_config.default;
    let backend_config = llm_config
        .backends
        .get(backend_name)
        .ok_or_else(|| anyhow!("default backend '{}' not found in config", backend_name))?;

    create_backend(backend_name, backend_config)
}

/// MultiBackend routes LLM requests to different backends based on use case
pub struct MultiBackend {
    /// Map from use case name to backend
    use_case_backends: RwLock<HashMap<String, Arc<dyn LlmBackend>>>,
    /// Default backend for unknown use cases
    default_backend: Arc<dyn LlmBackend>,
}

impl MultiBackend {
    /// Create a MultiBackend from LLM config
    pub fn new(llm_config: &LlmConfig) -> Result<Self> {
        let default_backend = create_default_backend(llm_config)?;

        let use_case_backends = RwLock::new(HashMap::new());

        // Create backends for each use case
        for (use_case_name, backend_name) in &llm_config.use_cases {
            if let Some(backend_config) = llm_config.backends.get(backend_name) {
                let backend = create_backend(backend_name, backend_config)?;
                use_case_backends
                    .write()
                    .map_err(|_| anyhow!("failed to acquire write lock"))?
                    .insert(use_case_name.clone(), backend);
            } else {
                tracing::warn!(
                    "backend '{}' not found for use case '{}', will use default",
                    backend_name,
                    use_case_name
                );
            }
        }

        Ok(Self {
            use_case_backends,
            default_backend,
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
        };

        let backend = create_backend("test", &config).unwrap();
        assert_eq!(backend.name(), "anthropic");
    }

    #[test]
    fn test_create_openai_compat_backend() {
        let config = LlmBackendConfig {
            backend_type: "openai-compat".to_string(),
            model: "gpt-4".to_string(),
            api_key_cmd: Some("echo sk-test-key".to_string()),
            base_url: Some("https://api.openai.com/v1".to_string()),
        };

        let backend = create_backend("test", &config).unwrap();
        assert_eq!(backend.name(), "openai_compat");
    }

    #[test]
    fn test_create_openai_compat_without_base_url_fails() {
        let config = LlmBackendConfig {
            backend_type: "openai-compat".to_string(),
            model: "gpt-4".to_string(),
            api_key_cmd: Some("echo sk-test-key".to_string()),
            base_url: None,
        };

        let result = create_backend("test", &config);
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
        };

        let result = create_backend("test", &config);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("unknown backend"));
    }
}
