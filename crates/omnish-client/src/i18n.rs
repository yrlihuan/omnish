//! Lightweight i18n support for UI-facing strings.
//!
//! Language files are embedded at compile time. Call `init("zh")` to switch
//! to Simplified Chinese, `init("zh-tw")` for Traditional Chinese;
//! the default is English. Use `t("key")` to look up a string,
//! falling back to the key itself if not found.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

static EN: &str = include_str!("i18n/en.json");
static ZH: &str = include_str!("i18n/zh.json");
static ZH_TW: &str = include_str!("i18n/zh-tw.json");

static EN_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
static ZH_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
static ZH_TW_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();

/// 0 = en, 1 = zh, 2 = zh-tw
static LANG: AtomicU8 = AtomicU8::new(0);

fn parse(json: &str) -> HashMap<String, String> {
    serde_json::from_str(json).unwrap_or_default()
}

fn en_map() -> &'static HashMap<String, String> {
    EN_MAP.get_or_init(|| parse(EN))
}

fn zh_map() -> &'static HashMap<String, String> {
    ZH_MAP.get_or_init(|| parse(ZH))
}

fn zh_tw_map() -> &'static HashMap<String, String> {
    ZH_TW_MAP.get_or_init(|| parse(ZH_TW))
}

/// Set the active language. Accepts "zh", "chinese", "cn" for Simplified Chinese;
/// "zh-tw", "zh-hant", "zht" for Traditional Chinese;
/// everything else defaults to English.
pub fn init(lang: &str) {
    let code = match lang.to_lowercase().as_str() {
        "zh" | "chinese" | "cn" | "中文" | "简体中文" => 1,
        "zh-tw" | "zh-hant" | "zht" | "繁體中文" => 2,
        _ => 0,
    };
    LANG.store(code, Ordering::Relaxed);
    // Eagerly initialize the maps
    en_map();
    zh_map();
    zh_tw_map();
}

/// Look up a translation key. Returns the translated string, or the key
/// itself as fallback (so English works even without a lookup).
pub fn t(key: &str) -> &str {
    let map = match LANG.load(Ordering::Relaxed) {
        1 => zh_map(),
        2 => zh_tw_map(),
        _ => en_map(),
    };
    map.get(key).map(|s| s.as_str()).unwrap_or(key)
}

/// Look up a translation key and replace `{placeholder}` tokens.
/// Accepts a slice of `(placeholder, value)` pairs.
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key).to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{}}}", k), v);
    }
    s
}

/// Translate a config schema label. Uses the English label as the lookup key
/// (prefixed with "config.") after normalizing to snake_case.
/// Returns the translated label, or the original if no translation found.
pub fn translate_label(label: &str) -> String {
    // Try direct key lookup first: "config.<snake_case>"
    let key = format!("config.{}", label_to_key(label));
    let translated = t(&key);
    if translated != key {
        return translated.to_string();
    }
    // Fallback: return original label
    label.to_string()
}

/// Convert a label like "Ghost text timeout (ms)" to "ghost_text_timeout_ms".
fn label_to_key(label: &str) -> String {
    label
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that depend on global LANG state must run sequentially in one test.
    #[test]
    fn test_translations() {
        // English
        init("en");
        assert_eq!(t("on"), "[ON]");
        assert_eq!(t("off"), "[OFF]");
        assert_eq!(t("confirm"), "confirm");
        assert_eq!(t("nonexistent.key"), "nonexistent.key");
        assert_eq!(tf("error.failed_delete_conversation", &[("n", "3")]),
                   "Failed to delete conversation [3]");

        // Simplified Chinese
        init("zh");
        assert_eq!(t("on"), "[开]");
        assert_eq!(t("off"), "[关]");
        assert_eq!(t("confirm"), "确认");
        assert_eq!(translate_label("Completion enabled"), "启用补全");
        assert_eq!(translate_label("Ghost text timeout (ms)"), "幽灵文本超时 (毫秒)");
        assert_eq!(translate_label("LLM"), "大语言模型");

        // Traditional Chinese
        init("zh-tw");
        assert_eq!(t("on"), "[開]");
        assert_eq!(t("off"), "[關]");
        assert_eq!(t("confirm"), "確認");
        assert_eq!(translate_label("Completion enabled"), "啟用補全");

        // Reset to English
        init("en");
    }

    #[test]
    fn test_label_to_key() {
        assert_eq!(label_to_key("Ghost text timeout (ms)"), "ghost_text_timeout_ms");
        assert_eq!(label_to_key("Completion enabled"), "completion_enabled");
        assert_eq!(label_to_key("API key"), "api_key");
    }
}
