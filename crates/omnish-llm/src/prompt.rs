/// Embedded chat prompt JSON, compiled into the binary.
pub const CHAT_PROMPT_JSON: &str = include_str!("../assets/chat.json");

/// Example template for chat.override.json (written to ~/.omnish/ if not present).
pub const CHAT_OVERRIDE_EXAMPLE: &str = include_str!("../assets/chat.override.json.example");

/// Manages system prompt as composable named fragments.
pub struct PromptManager {
    fragments: Vec<(String, String)>,
}

#[derive(serde::Deserialize)]
struct Fragment {
    name: String,
    content: FragmentContent,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum FragmentContent {
    Single(String),
    Lines(Vec<String>),
}

impl FragmentContent {
    fn into_string(self) -> String {
        match self {
            Self::Single(s) => s,
            Self::Lines(lines) => lines.join("\n"),
        }
    }
}

impl Default for PromptManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptManager {
    pub fn new() -> Self {
        Self {
            fragments: Vec::new(),
        }
    }

    /// Add a named fragment. Fragments are joined in insertion order.
    pub fn add(&mut self, name: &str, content: &str) {
        self.fragments.push((name.to_string(), content.to_string()));
    }

    /// Build the final system prompt by joining all fragments.
    pub fn build(&self) -> String {
        self.fragments
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Load fragments from JSON (array of {name, content} objects).
    pub fn from_json(json: &str) -> Result<Self, String> {
        let fragments: Vec<Fragment> =
            serde_json::from_str(json).map_err(|e| format!("invalid prompt JSON: {}", e))?;
        let mut pm = Self::new();
        for f in fragments {
            pm.add(&f.name, &f.content.into_string());
        }
        Ok(pm)
    }

    /// Merge overrides into this manager. Fragments with matching names are
    /// replaced; unmatched override fragments are appended.
    pub fn merge(mut self, overrides: Self) -> Self {
        for (name, content) in overrides.fragments {
            if let Some(existing) = self.fragments.iter_mut().find(|(n, _)| n == &name) {
                existing.1 = content;
            } else {
                self.fragments.push((name, content));
            }
        }
        self
    }

    /// Create a PromptManager with default chat fragments from the embedded JSON.
    pub fn default_chat() -> Self {
        Self::from_json(CHAT_PROMPT_JSON).expect("embedded chat.json is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_chat_contains_all_sections() {
        let pm = PromptManager::default_chat();
        let prompt = pm.build();
        assert!(prompt.contains("omnish assistant"));
        assert!(prompt.contains("Tone and style"));
        assert!(prompt.contains("Professional objectivity"));
        assert!(prompt.contains("Tool usage"));
        assert!(prompt.contains("command_query"));
        assert!(prompt.contains("Doing tasks"));
    }

    #[test]
    fn test_from_json_single_string() {
        let json = r#"[{"name": "a", "content": "hello world"}]"#;
        let pm = PromptManager::from_json(json).unwrap();
        assert_eq!(pm.build(), "hello world");
    }

    #[test]
    fn test_from_json_lines_array() {
        let json = r#"[{"name": "a", "content": ["line1", "line2"]}]"#;
        let pm = PromptManager::from_json(json).unwrap();
        assert_eq!(pm.build(), "line1\nline2");
    }

    #[test]
    fn test_from_json_preserves_order() {
        let json = r#"[{"name": "b", "content": "second"}, {"name": "a", "content": "first"}]"#;
        let pm = PromptManager::from_json(json).unwrap();
        assert_eq!(pm.build(), "second\n\nfirst");
    }

    #[test]
    fn test_from_json_invalid() {
        assert!(PromptManager::from_json("not json").is_err());
    }

    #[test]
    fn test_add_and_build() {
        let mut pm = PromptManager::new();
        pm.add("a", "hello");
        pm.add("b", "world");
        assert_eq!(pm.build(), "hello\n\nworld");
    }

    #[test]
    fn test_empty_build() {
        let pm = PromptManager::new();
        assert_eq!(pm.build(), "");
    }

    #[test]
    fn test_merge_replaces_matching() {
        let mut base = PromptManager::new();
        base.add("a", "original_a");
        base.add("b", "original_b");

        let mut overrides = PromptManager::new();
        overrides.add("a", "replaced_a");

        let merged = base.merge(overrides);
        assert_eq!(merged.build(), "replaced_a\n\noriginal_b");
    }

    #[test]
    fn test_merge_appends_new() {
        let mut base = PromptManager::new();
        base.add("a", "content_a");

        let mut overrides = PromptManager::new();
        overrides.add("b", "content_b");

        let merged = base.merge(overrides);
        assert_eq!(merged.build(), "content_a\n\ncontent_b");
    }
}
