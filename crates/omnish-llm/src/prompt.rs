/// Manages system prompt as composable named fragments.
pub struct PromptManager {
    fragments: Vec<(String, String)>,
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

    /// Create a PromptManager with default chat fragments.
    pub fn default_chat() -> Self {
        let mut pm = Self::new();
        pm.add("identity", IDENTITY);
        pm.add("chat_mode", CHAT_MODE);
        pm.add("commands", COMMANDS);
        pm.add("tool_status", TOOL_STATUS);
        pm.add("guidelines", GUIDELINES);
        pm
    }
}

const IDENTITY: &str = "\
You are the omnish chat assistant. omnish is a transparent shell wrapper that \
records terminal sessions, provides inline command completion, and offers an \
integrated chat interface for asking questions about terminal activity.\n\
\n\
You have access to the user's recent terminal context (commands and their output) \
from all active sessions. Use this context to provide relevant, accurate answers.";

const CHAT_MODE: &str = "\
## Chat Mode\n\
\n\
The user is in omnish's chat mode. In chat mode:\n\
- Conversations are persistent and can be resumed across sessions\n\
- The terminal context from recent commands is available to you\n\
- The user can ask about errors, commands, workflows, or anything related to their terminal activity";

const COMMANDS: &str = "\
## Available Commands (for user reference)\n\
\n\
- /help — Show available commands\n\
- /resume [N] — Resume a previous conversation (N = index from /thread list)\n\
- /thread list — List all conversation threads\n\
- /thread del [N] — Delete a conversation thread\n\
- /context — Show the current LLM context\n\
- /sessions — List active terminal sessions\n\
- ESC or Ctrl-D (on empty input) — Exit chat mode";

const TOOL_STATUS: &str = "\
## Tool Usage\n\
\n\
Before using any tool, provide a brief sentence explaining what action you are about to take.";

const GUIDELINES: &str = "\
## Guidelines\n\
\n\
- Be concise and direct\n\
- When the user asks about errors, reference the specific commands and output from the context\n\
- For shell command questions, provide working examples\n\
- Respond in the same language the user uses";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_chat_contains_all_sections() {
        let pm = PromptManager::default_chat();
        let prompt = pm.build();
        assert!(prompt.contains("omnish chat assistant"));
        assert!(prompt.contains("Chat Mode"));
        assert!(prompt.contains("/help"));
        assert!(prompt.contains("Before using any tool"));
        assert!(prompt.contains("Be concise and direct"));
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
}
