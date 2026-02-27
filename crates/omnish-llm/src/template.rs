/// Build the user-content prompt sent to the LLM.
pub fn build_user_content(context: &str, query: Option<&str>) -> String {
    if let Some(q) = query {
        format!(
            "Here is the terminal session context:\n\n```\n{}\n```\n\nUser question: {}",
            context, q
        )
    } else {
        format!(
            "Analyze this terminal session output and explain any errors or issues:\n\n```\n{}\n```",
            context
        )
    }
}

/// Build the user-content prompt for shell command completion.
pub fn build_completion_content(context: &str, input: &str, cursor_pos: usize) -> String {
    if input.is_empty() {
        format!(
            "Here is the terminal session context (most recent commands last):\n\n\
             ```\n{}\n```\n\n\
             The user just returned to the shell prompt. \
             Predict the next command they are most likely to type.\n\
             Pay close attention to the most recent commands and their output — \
             infer what the user is trying to accomplish and what logical next step follows.\n\n\
             Reply with a JSON array:\n\
             [{{\"text\": \"<full command>\", \"confidence\": <0.0-1.0>}}]\n\
             Return at most 3 suggestions sorted by confidence descending.\n\
             Return [] if no good prediction exists.\n\
             Do not include any other text outside the JSON array.",
            context
        )
    } else {
        format!(
            "Here is the terminal session context (most recent commands last):\n\n\
             ```\n{}\n```\n\n\
             The user is typing a shell command. Current input: `{}`\n\
             Cursor position: {}\n\
             Use the recent commands and their output to understand what the user is doing, \
             then suggest the most likely completion.\n\n\
             Reply with a JSON array containing the FULL completed command (including the user's current input as prefix):\n\
             [{{\"text\": \"<full command including prefix>\", \"confidence\": <0.0-1.0>}}]\n\
             Return at most 3 suggestions sorted by confidence descending.\n\
             Return [] if no good completion exists.\n\
             Do not include any other text outside the JSON array.",
            context, input, cursor_pos
        )
    }
}

/// Build the user-content prompt for shell command completion (up to 2 suggestions, JSON array).
pub fn build_simple_completion_content(context: &str, input: &str, cursor_pos: usize) -> String {
    if input.is_empty() {
        format!(
            "Here is the terminal session context (most recent commands last):\n\n\
             ```\n{}\n```\n\n\
             The user just returned to the shell prompt. \
             Predict the next command they are most likely to type.\n\
             Pay close attention to the most recent commands and their output — \
             infer what the user is trying to accomplish and what logical next step follows.\n\n\
             Reply with a JSON array of up to 2 suggestions (most likely first):\n\
             [\"<completion1>\", \"<completion2>\"]\n\
             Return [] if no good prediction exists.\n\
             Do not include any other text outside the JSON array.",
            context
        )
    } else {
        format!(
            "Here is the terminal session context (most recent commands last):\n\n\
             ```\n{}\n```\n\n\
             The user is typing a shell command. Current input: `{}`\n\
             Cursor position: {}\n\
             Use the recent commands and their output to understand what the user is doing, \
             then suggest the most likely completion.\n\n\
             Reply with a JSON array of up to 2 FULL commands (including the user's current input as prefix):\n\
             [\"<full command including prefix>\"]\n\
             Return [] if no good completion exists.\n\
             Do not include any other text outside the JSON array.",
            context, input, cursor_pos
        )
    }
}

/// Return the prompt template with `{context}` and `{query}` placeholders.
pub fn prompt_template(has_query: bool) -> &'static str {
    if has_query {
        "Here is the terminal session context:\n\n```\n{context}\n```\n\nUser question: {query}"
    } else {
        "Analyze this terminal session output and explain any errors or issues:\n\n```\n{context}\n```"
    }
}

/// The daily-notes LLM summary prompt.
pub const DAILY_NOTES_PROMPT: &str =
    "请用中文简要总结今天的工作内容，包括主要活动和成果，2-3段即可。";

/// Known template names for `/template <name>`.
pub const TEMPLATE_NAMES: &[&str] = &["chat", "auto-complete", "daily-notes"];

/// Return a named template with placeholders for inspection.
/// Returns `None` if the name is unknown.
pub fn template_by_name(name: &str) -> Option<String> {
    match name {
        "chat" => Some(format!(
            "--- chat (with query) ---\n{}\n\n--- chat (auto-analyze) ---\n{}",
            prompt_template(true),
            prompt_template(false),
        )),
        "auto-complete" => Some(format!(
            "--- auto-complete (empty input → predict next command) ---\n{}\n\n\
             --- auto-complete (partial input → complete command) ---\n{}",
            build_simple_completion_content("{context}", "", 0),
            build_simple_completion_content("{context}", "{input}", 0),
        )),
        "daily-notes" => Some(DAILY_NOTES_PROMPT.to_string()),
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
    fn test_template_by_name_unknown() {
        assert!(template_by_name("nonexistent").is_none());
    }
}
