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

/// The daily-notes LLM summary prompt.
pub const DAILY_NOTES_PROMPT: &str =
    "以下<commands>中是从多台终端收集的过去24小时的命令及其简要输出，\
     <hourly_summaries>中是各小时的工作摘要（如有）。\
     请用中文以项目符号列表形式列出今天的工作内容，每个条目包含一项主要活动或成果。适合直接作为工作日志。";

/// The hourly-notes LLM summary prompt.
pub const HOURLY_NOTES_PROMPT: &str =
    "以下<commands>中是从多台终端收集的过去1小时的命令及其简要输出。\
     请用中文以项目符号列表形式列出这一个小时的工作内容，每个条目包含一项主要活动或成果。适合直接作为工作日志。";

/// The thread-title LLM prompt — generates a short title for the thread.
pub const THREAD_SUMMARY_PROMPT: &str =
    "以下<conversation>中是一段用户与AI助手的对话记录。\
     请用中文生成一个简短标题（不超过20字），概括这段对话的主题。\
     只输出标题本身，不要加引号或前缀。";

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
