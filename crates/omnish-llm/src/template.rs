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

/// Build the user-content prompt for shell command completion (up to 2 suggestions, JSON array).
///
/// Instructions are placed first, then context, then input — this ordering maximizes
/// prefix stability across consecutive requests for better KV cache hit rates.
pub fn build_simple_completion_content(context: &str, input: &str, cursor_pos: usize) -> String {
    // Unified template: instructions + context form a stable prefix for KV cache,
    // only the trailing input line varies between requests.
    let input_line = if input.is_empty() {
        "Current input: (empty — user just returned to the shell prompt)".to_string()
    } else {
        format!("Current input: `{}`\nCursor position: {}", input, cursor_pos)
    };
    format!(
        "You are a shell command completion engine.\n\
         Use <recent> and their output to understand what the user is doing, \
         then predict or complete the command.\n\n\
         Reply with a JSON array of up to 2 FULL commands:\n\
         [\"<command1>\", \"<command2>\"]\n\
         - 1st: the most likely completion (only if high confidence).\n\
         - 2nd: a longer command that completes the entire task end-to-end. \n\
         Do NOT include `&&` unless the user input already contains `&&`.\n\
         Return [] if no good completion exists.\n\
         Do not include any other text outside the JSON array.\n\n\
         {}\n\n\
         {}",
        context, input_line
    )
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
/// `language` should be "en", "zh", or "zh-tw".
pub fn append_language_instruction(prompt: &str, language: &str) -> String {
    let instruction = match language {
        "zh" => "请用中文回答。",
        "zh-tw" => "請用繁體中文回答。",
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
        "auto-complete" => Some(build_simple_completion_content("{context}", "{input}", 0)),
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
    fn test_simple_completion_instructions_before_context() {
        let context = "$ ls\nfile.txt";
        let result = build_simple_completion_content(context, "git", 3);
        let instructions_pos = result.find("You are a shell command completion engine").unwrap();
        let context_pos = result.find(context).unwrap();
        let input_pos = result.find("Current input: `git`").unwrap();
        assert!(instructions_pos < context_pos,
            "Instructions should appear before context");
        assert!(context_pos < input_pos,
            "Context should appear before input");
    }

    #[test]
    fn test_simple_completion_empty_input_instructions_first() {
        let context = "$ ls\nfile.txt";
        let result = build_simple_completion_content(context, "", 0);
        let instructions_pos = result.find("You are a shell command completion engine").unwrap();
        let context_pos = result.find(context).unwrap();
        assert!(instructions_pos < context_pos,
            "Instructions should appear before context for empty input too");
    }

    /// KV cache stability: empty-input and non-empty-input prompts must share
    /// the same prefix up to (and including) the context, so the LLM server
    /// can reuse cached KV state from warmup requests.
    #[test]
    fn test_simple_completion_prefix_stable_across_inputs() {
        let context = "<history>\nls\ngit status\n</history>";
        let empty = build_simple_completion_content(context, "", 0);
        let typed = build_simple_completion_content(context, "git", 3);
        // Find where context ends in both strings
        let ctx_end_empty = empty.find(context).unwrap() + context.len();
        let ctx_end_typed = typed.find(context).unwrap() + context.len();
        assert_eq!(&empty[..ctx_end_empty], &typed[..ctx_end_typed],
            "Instruction + context prefix must be identical for KV cache reuse");
    }
}
