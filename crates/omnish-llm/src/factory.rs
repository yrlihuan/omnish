use crate::anthropic::AnthropicBackend;
use crate::backend::LlmBackend;
use crate::openai_compat::OpenAiCompatBackend;
use anyhow::{anyhow, Result};
use omnish_common::config::{LlmBackendConfig, LlmConfig};
use std::process::Command;
use std::sync::Arc;

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
        "anthropic" => Ok(Arc::new(AnthropicBackend {
            api_key,
            model: config.model.clone(),
        })),
        "openai-compat" => {
            let base_url = config
                .base_url
                .clone()
                .ok_or_else(|| anyhow!("openai-compat requires base_url"))?;
            Ok(Arc::new(OpenAiCompatBackend {
                api_key,
                model: config.model.clone(),
                base_url,
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
