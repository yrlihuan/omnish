//! Model presets loaded from embedded JSON.
//!
//! Provides provider metadata (backend_type, base_url, default_model,
//! context_window) for both the daemon and install.sh.

use serde::Deserialize;
use std::collections::HashMap;

const PRESETS_JSON: &str = include_str!("../assets/model_presets.json");

#[derive(Debug, Deserialize)]
pub struct ProviderPreset {
    pub backend_type: String,
    pub base_url: String,
    pub default_model: String,
    pub context_window: usize,
}

#[derive(Debug, Deserialize)]
struct PresetsFile {
    providers: HashMap<String, ProviderPreset>,
    chat_providers: Vec<String>,
    completion_providers: Vec<String>,
}

/// Parsed presets singleton.
fn presets() -> &'static PresetsFile {
    use std::sync::OnceLock;
    static PRESETS: OnceLock<PresetsFile> = OnceLock::new();
    PRESETS.get_or_init(|| {
        serde_json::from_str(PRESETS_JSON).expect("model_presets.json is invalid")
    })
}

/// Get a provider preset by name.
pub fn get_provider(name: &str) -> Option<&'static ProviderPreset> {
    presets().providers.get(name)
}

/// Default context_window (tokens) for a provider.
pub fn default_context_window(provider: &str) -> Option<usize> {
    get_provider(provider).map(|p| p.context_window)
}

/// List all provider names.
pub fn provider_names() -> Vec<&'static str> {
    presets().providers.keys().map(|s| s.as_str()).collect()
}

/// Chat provider list (ordered).
pub fn chat_providers() -> &'static [String] {
    &presets().chat_providers
}

/// Completion provider list (ordered).
pub fn completion_providers() -> &'static [String] {
    &presets().completion_providers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_presets() {
        let p = get_provider("anthropic").unwrap();
        assert_eq!(p.backend_type, "anthropic");
        assert_eq!(p.default_model, "claude-sonnet-4-20250514");
        assert!(p.context_window > 0);
    }

    #[test]
    fn test_unknown_provider() {
        assert!(get_provider("nonexistent").is_none());
    }

    #[test]
    fn test_chat_providers_non_empty() {
        assert!(!chat_providers().is_empty());
    }

    #[test]
    fn test_completion_providers_non_empty() {
        assert!(!completion_providers().is_empty());
    }
}
