/// Result of parsing a chat message for `/` commands.
pub enum ChatAction {
    /// A `/` command was recognized. Contains the result text and optional redirect path.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    Command {
        result: String,
        redirect: Option<String>,
    },
    /// Not a command — forward as normal LLM query.
    LlmQuery(String),
    /// A `/` command that needs daemon data. Contains the debug query to send and optional redirect.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    DaemonDebug {
        query: String,
        redirect: Option<String>,
    },
}

/// Parse redirect suffix: "some text > /path/to/file" -> ("some text", Some("/path/to/file"))
#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn parse_redirect(input: &str) -> (&str, Option<&str>) {
    if let Some(pos) = input.rfind(" > ") {
        let path = input[pos + 3..].trim();
        if !path.is_empty() {
            return (&input[..pos], Some(path));
        }
    }
    (input, None)
}

/// Dispatch a chat message. Returns ChatAction describing what to do.
#[cfg(debug_assertions)]
pub fn dispatch(msg: &str) -> ChatAction {
    if !msg.starts_with('/') {
        return ChatAction::LlmQuery(msg.to_string());
    }

    let (cmd_str, redirect) = parse_redirect(msg);
    let redirect = redirect.map(|s| s.to_string());
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();

    match parts.first().map(|s| *s) {
        Some("/debug") => handle_debug(&parts[1..], redirect),
        _ => ChatAction::LlmQuery(msg.to_string()), // unknown /cmd -> LLM
    }
}

/// In release builds, all chat messages go to LLM.
#[cfg(not(debug_assertions))]
pub fn dispatch(msg: &str) -> ChatAction {
    ChatAction::LlmQuery(msg.to_string())
}

#[cfg(debug_assertions)]
fn handle_debug(args: &[&str], redirect: Option<String>) -> ChatAction {
    match args.first().map(|s| *s) {
        Some("context") => ChatAction::DaemonDebug {
            query: "__debug:context".to_string(),
            redirect,
        },
        Some("template") => {
            let result = omnish_llm::template::prompt_template(true).to_string();
            ChatAction::Command { result, redirect }
        }
        Some(other) => ChatAction::Command {
            result: format!("Unknown debug subcommand: {}", other),
            redirect: None,
        },
        None => ChatAction::Command {
            result: "Usage: /debug <context|template> [> file.txt]".to_string(),
            redirect: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_redirect() {
        assert_eq!(parse_redirect("/debug context"), ("/debug context", None));
        assert_eq!(
            parse_redirect("/debug context > /tmp/out.txt"),
            ("/debug context", Some("/tmp/out.txt"))
        );
    }

    #[test]
    fn test_non_command_is_llm_query() {
        match dispatch("what is this error?") {
            ChatAction::LlmQuery(q) => assert_eq!(q, "what is this error?"),
            _ => panic!("expected LlmQuery"),
        }
    }

    #[test]
    fn test_debug_context_dispatches_to_daemon() {
        match dispatch("/debug context") {
            ChatAction::DaemonDebug { query, redirect } => {
                assert_eq!(query, "__debug:context");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonDebug"),
        }
    }

    #[test]
    fn test_debug_context_with_redirect() {
        match dispatch("/debug context > /tmp/ctx.txt") {
            ChatAction::DaemonDebug { query, redirect } => {
                assert_eq!(query, "__debug:context");
                assert_eq!(redirect.as_deref(), Some("/tmp/ctx.txt"));
            }
            _ => panic!("expected DaemonDebug"),
        }
    }

    #[test]
    fn test_debug_template_is_local() {
        match dispatch("/debug template") {
            ChatAction::Command { result, redirect } => {
                assert!(result.contains("{context}"));
                assert!(redirect.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_debug_no_args_shows_usage() {
        match dispatch("/debug") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("Usage"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_unknown_slash_command_falls_through() {
        match dispatch("/unknown foo") {
            ChatAction::LlmQuery(q) => assert_eq!(q, "/unknown foo"),
            _ => panic!("expected LlmQuery"),
        }
    }
}
