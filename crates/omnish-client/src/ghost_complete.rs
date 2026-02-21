/// Trait for completion data sources. Providers are queried in order; first match wins.
pub trait CompletionProvider {
    /// Given current input text (after the `:` prefix), return a full-line suggestion.
    /// Returns None if no completion available.
    /// The suggestion MUST start with `input` as a prefix.
    fn suggest(&self, input: &str) -> Option<String>;
}

/// Completes omnish built-in `/` commands.
pub struct BuiltinProvider {
    commands: Vec<String>,
}

impl BuiltinProvider {
    pub fn new() -> Self {
        Self {
            commands: vec![
                "/debug".to_string(),
                "/debug context".to_string(),
                "/debug template".to_string(),
            ],
        }
    }
}

impl CompletionProvider for BuiltinProvider {
    fn suggest(&self, input: &str) -> Option<String> {
        if input.is_empty() {
            return None;
        }
        // Find first command that starts with input and is longer than input
        self.commands
            .iter()
            .find(|cmd| cmd.starts_with(input) && cmd.len() > input.len())
            .cloned()
    }
}

/// Manages ghost text completion state.
pub struct GhostCompleter {
    providers: Vec<Box<dyn CompletionProvider>>,
    /// The full suggestion text (including the input prefix)
    current_suggestion: Option<String>,
    /// Length of the input that produced current suggestion
    current_input_len: usize,
}

impl GhostCompleter {
    pub fn new(providers: Vec<Box<dyn CompletionProvider>>) -> Self {
        Self {
            providers,
            current_suggestion: None,
            current_input_len: 0,
        }
    }

    /// Update with new input. Returns the ghost suffix to display, or None.
    pub fn update(&mut self, input: &str) -> Option<&str> {
        self.current_suggestion = None;
        self.current_input_len = input.len();

        for provider in &self.providers {
            if let Some(suggestion) = provider.suggest(input) {
                if suggestion.len() > input.len() {
                    self.current_suggestion = Some(suggestion);
                    break;
                }
            }
        }

        self.ghost_suffix()
    }

    /// Get the current ghost suffix (the part after what user typed).
    fn ghost_suffix(&self) -> Option<&str> {
        self.current_suggestion
            .as_deref()
            .map(|s| &s[self.current_input_len..])
            .filter(|s| !s.is_empty())
    }

    /// Accept the current ghost. Returns the suffix to append to the buffer.
    pub fn accept(&mut self) -> Option<String> {
        let suffix = self.ghost_suffix().map(|s| s.to_string());
        self.current_suggestion = None;
        suffix
    }

    /// Clear any active ghost text.
    pub fn clear(&mut self) {
        self.current_suggestion = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_provider_exact_match_no_ghost() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/debug context"), None);
    }

    #[test]
    fn test_builtin_provider_prefix_match() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/deb"), Some("/debug".to_string()));
    }

    #[test]
    fn test_builtin_provider_subcommand_match() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/debug con"), Some("/debug context".to_string()));
    }

    #[test]
    fn test_builtin_provider_no_match() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest("/xyz"), None);
    }

    #[test]
    fn test_builtin_provider_empty_input() {
        let p = BuiltinProvider::new();
        assert_eq!(p.suggest(""), None);
    }

    #[test]
    fn test_completer_update_returns_ghost_suffix() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        assert_eq!(c.update("/deb"), Some("ug"));
    }

    #[test]
    fn test_completer_update_no_match_returns_none() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        assert_eq!(c.update("hello world"), None);
    }

    #[test]
    fn test_completer_accept_returns_suffix() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        c.update("/deb");
        assert_eq!(c.accept(), Some("ug".to_string()));
        assert_eq!(c.accept(), None);
    }

    #[test]
    fn test_completer_clear() {
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        c.update("/deb");
        c.clear();
        assert_eq!(c.accept(), None);
    }

    #[test]
    fn test_completer_first_provider_wins() {
        struct AlwaysHello;
        impl CompletionProvider for AlwaysHello {
            fn suggest(&self, input: &str) -> Option<String> {
                if !input.is_empty() {
                    Some(format!("{}hello", input))
                } else {
                    None
                }
            }
        }
        let providers: Vec<Box<dyn CompletionProvider>> = vec![
            Box::new(AlwaysHello),
            Box::new(BuiltinProvider::new()),
        ];
        let mut c = GhostCompleter::new(providers);
        assert_eq!(c.update("/deb"), Some("hello"));
    }
}
