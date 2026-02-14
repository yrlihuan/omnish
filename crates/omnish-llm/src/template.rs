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
}
