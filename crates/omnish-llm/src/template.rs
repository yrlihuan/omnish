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
             Reply with a JSON array:\n\
             [{{\"text\": \"<text after cursor>\", \"confidence\": <0.0-1.0>}}]\n\
             Return at most 3 suggestions sorted by confidence descending.\n\
             Return [] if no good completion exists.\n\
             Do not include any other text outside the JSON array.",
            context, input, cursor_pos
        )
    }
}

/// Build the user-content prompt for simple shell command completion (single suggestion, plain text).
pub fn build_simple_completion_content(context: &str, input: &str, cursor_pos: usize) -> String {
    if input.is_empty() {
        format!(
            "Here is the terminal session context (most recent commands last):\n\n\
             ```\n{}\n```\n\n\
             The user just returned to the shell prompt. \
             Predict the next command they are most likely to type.\n\
             Pay close attention to the most recent commands and their output — \
             infer what the user is trying to accomplish and what logical next step follows.\n\n\
             Output ONLY the suggested completion text (the part that would come after the cursor).\n\
             If you have no suggestion, output an empty string.\n\
             Do not include any explanation, formatting, or additional text.",
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
             Output ONLY the suggested completion text (the part that would come after the cursor).\n\
             If you have no suggestion, output an empty string.\n\
             Do not include any explanation, formatting, or additional text.",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_user_content_with_query() {
        let result = build_user_content("$ ls\nfoo bar", Some("what files are here?"));
        assert!(result.contains("$ ls\nfoo bar"));
        assert!(result.contains("User question: what files are here?"));
    }

    #[test]
    fn test_build_user_content_without_query() {
        let result = build_user_content("$ exit 1", None);
        assert!(result.contains("$ exit 1"));
        assert!(result.contains("Analyze this terminal session"));
        assert!(!result.contains("User question"));
    }

    #[test]
    fn test_build_completion_content() {
        let result = build_completion_content("$ ls\nfoo bar", "git sta", 7);
        assert!(result.contains("$ ls\nfoo bar"));
        assert!(result.contains("Current input: `git sta`"));
        assert!(result.contains("Cursor position: 7"));
        assert!(result.contains("JSON array"));
    }

    #[test]
    fn test_build_completion_content_empty_input() {
        let result = build_completion_content("$ ls\nfoo bar", "", 0);
        assert!(result.contains("$ ls\nfoo bar"));
        assert!(result.contains("Predict the next command"));
        assert!(!result.contains("Current input"));
        assert!(result.contains("JSON array"));
    }

    #[test]
    fn test_prompt_template_with_query() {
        let t = prompt_template(true);
        assert!(t.contains("{context}"));
        assert!(t.contains("{query}"));
    }

    #[test]
    fn test_prompt_template_without_query() {
        let t = prompt_template(false);
        assert!(t.contains("{context}"));
        assert!(!t.contains("{query}"));
    }

    #[test]
    fn test_build_simple_completion_content() {
        let result = build_simple_completion_content("$ ls\nfoo bar", "git sta", 7);
        assert!(result.contains("$ ls\nfoo bar"));
        assert!(result.contains("Current input: `git sta`"));
        assert!(result.contains("Cursor position: 7"));
        assert!(result.contains("Output ONLY the suggested completion text"));
        assert!(result.contains("If you have no suggestion, output an empty string"));
        assert!(!result.contains("JSON array"));
    }

    #[test]
    fn test_build_simple_completion_content_empty_input() {
        let result = build_simple_completion_content("$ ls\nfoo bar", "", 0);
        assert!(result.contains("$ ls\nfoo bar"));
        assert!(result.contains("Predict the next command"));
        assert!(result.contains("Output ONLY the suggested completion text"));
        assert!(result.contains("If you have no suggestion, output an empty string"));
        assert!(!result.contains("JSON array"));
    }
}
