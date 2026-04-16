/// Build the user-content prompt sent to the LLM.
pub fn build_user_content(context: &str, query: Option<&str>) -> String {
    if let Some(q) = query {
        format!(
            "Here is the terminal session context:\n\n{}\n\nUser question: {}",
            context, q
        )
    } else {
        format!(
            "Analyze this terminal session output and explain any errors or issues:\n\n{}",
            context
        )
    }
}

/// Static instruction text for completion requests (used as system prompt).
pub const COMPLETION_INSTRUCTIONS: &str = "\
You are a shell command completion engine.\n\
Use <recent> and their output to understand what the user is doing, \
then predict or complete the command.\n\n\
Reply with a JSON array of up to 2 FULL commands:\n\
[\"<command1>\", \"<command2>\"]\n\
- 1st: the most likely completion (only if high confidence).\n\
- 2nd: a longer command that completes the entire task end-to-end. \n\
Do NOT include `&&` unless the user input already contains `&&`.\n\
Return [] if no good completion exists.\n\
Do not include any other text outside the JSON array.";

/// Build the completion prompt parts: (system_prompt, user_input).
///
/// - system_prompt: static instructions (never changes, cached)
/// - user_input: current input line (changes every keystroke, not cached)
///
/// Context (command history) is passed separately via `LlmRequest.context` and
/// cached via Anthropic cache_control on a dedicated user message content block.
pub fn build_completion_parts(input: &str, cursor_pos: usize) -> (String, String) {
    let user = if input.is_empty() {
        "Current input: (empty — user just returned to the shell prompt)".to_string()
    } else {
        format!("Current input: `{}`\nCursor position: {}", input, cursor_pos)
    };
    (COMPLETION_INSTRUCTIONS.to_string(), user)
}

/// Return the prompt template with `{context}` and `{query}` placeholders.
pub fn prompt_template(has_query: bool) -> &'static str {
    if has_query {
        "Here is the terminal session context:\n\n{context}\n\nUser question: {query}"
    } else {
        "Analyze this terminal session output and explain any errors or issues:\n\n{context}"
    }
}

/// The daily-notes LLM summary prompt (English base).
pub const DAILY_NOTES_PROMPT: &str =
    "Below are <hourly_summaries> of today's work (each includes command logs, conversation logs, and summaries). \
     Based on this information, infer what projects and goals the user was actually working on today, \
     rather than simply listing which commands were run. \
     Summarize in bullet points, with each item describing a specific project activity or goal achieved. \
     Suitable as a work log directly.";

/// The periodic-summary LLM prompt template (English base).
pub const HOURLY_NOTES_PROMPT: &str =
    "Below are <commands> collected from multiple terminals over the past N hours (with brief output if available), \
     and <conversations> with the AI assistant (if available). \
     Based on this information, infer what projects and goals the user is actually working on, \
     rather than simply listing which commands were run. \
     Summarize in bullet points, with each item describing a specific project activity or goal achieved. \
     Suitable as a work log directly.";

/// The thread-title LLM prompt (English base) — generates a short title for the thread.
pub const THREAD_SUMMARY_PROMPT: &str =
    "Below is a <conversation> between a user and an AI assistant. \
     Generate a short title (no more than 10 words) that summarizes the topic of this conversation. \
     Output only the title itself, without quotes or prefixes.";

/// Append a language instruction to a prompt.
pub fn append_language_instruction(prompt: &str, language: &str) -> String {
    let instruction = match language {
        "zh" => "请用中文回答。",
        "zh-tw" => "請用繁體中文回答。",
        "ja" => "日本語で回答してください。",
        "ko" => "한국어로 답변해 주세요.",
        "fr" => "Répondez en français.",
        "es" => "Responde en español.",
        "ar" => "أجب باللغة العربية.",
        _ => "Respond in English.",
    };
    format!("{}\n\n{}", prompt, instruction)
}

/// Known template names for `/template <name>`.
pub const TEMPLATE_NAMES: &[&str] = &["chat", "chat-system", "auto-complete", "daily-notes", "hourly-notes"];

/// Return a named template with placeholders for inspection.
/// Returns `None` if the name is unknown.
pub fn template_by_name(name: &str) -> Option<String> {
    match name {
        "chat-system" => Some(crate::prompt::PromptManager::default_chat().build()),
        "chat" => Some("(handled by daemon — use /template chat)".to_string()),
        "auto-complete" => {
            let (sys, usr) = build_completion_parts("{input}", 0);
            Some(format!("[system]\n{}\n\n[user block 1 — cached]\n{{context}}\n\n[user block 2]\n{}", sys, usr))
        }
        "daily-notes" => Some(DAILY_NOTES_PROMPT.to_string()),
        "hourly-notes" => Some(HOURLY_NOTES_PROMPT.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_by_name_returns_some_for_known() {
        for name in TEMPLATE_NAMES {
            assert!(template_by_name(name).is_some(), "missing template: {}", name);
        }
    }

    #[test]
    fn test_chat_system_prompt_has_key_sections() {
        let prompt = crate::prompt::PromptManager::default_chat().build();
        assert!(prompt.contains("omnish assistant"));
        assert!(prompt.contains("Tool usage"));
        assert!(prompt.contains("command_query"));
    }

    #[test]
    fn test_template_by_name_unknown() {
        assert!(template_by_name("nonexistent").is_none());
    }

    #[test]
    fn test_completion_parts_instructions_are_static() {
        let (system, _) = build_completion_parts("git", 3);
        assert!(system.contains("You are a shell command completion engine"));
    }

    #[test]
    fn test_completion_parts_user_input() {
        let (_, user) = build_completion_parts("git", 3);
        assert!(user.contains("Current input: `git`"));
        assert!(user.contains("Cursor position: 3"));
    }

    #[test]
    fn test_completion_parts_empty_input() {
        let (_, user) = build_completion_parts("", 0);
        assert!(user.contains("empty"));
    }

    /// System prompt (instructions) is a constant — identical for every call.
    #[test]
    fn test_completion_instructions_static() {
        let (sys_a, _) = build_completion_parts("", 0);
        let (sys_b, _) = build_completion_parts("git", 3);
        assert_eq!(sys_a, sys_b,
            "Instructions must be identical regardless of input");
    }
}
