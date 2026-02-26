/// Result of parsing a chat message for `/` commands.
pub enum ChatAction {
    /// A `/` command was recognized. Contains the result text and optional redirect path.
    Command {
        result: String,
        redirect: Option<String>,
    },
    /// Not a command — forward as normal LLM query.
    LlmQuery(String),
    /// A `/` command that needs daemon data. Contains the query to send and optional redirect.
    DaemonQuery {
        query: String,
        redirect: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Command registry
// ---------------------------------------------------------------------------

enum CommandKind {
    /// Client-side command — handler returns result text.
    Local(fn() -> String),
    /// Forwarded to daemon as `__cmd:{key}`.
    Daemon(&'static str),
}

struct CommandEntry {
    /// Full command path, e.g. "/debug context".
    path: &'static str,
    kind: CommandKind,
    #[allow(dead_code)]
    help: &'static str,
}

fn debug_usage() -> String {
    let subs: Vec<&str> = COMMANDS
        .iter()
        .filter(|e| e.path.starts_with("/debug "))
        .map(|e| {
            let sub = &e.path["/debug ".len()..];
            sub
        })
        .collect();
    format!("Usage: /debug <{}> [> file.txt]", subs.join("|"))
}

fn debug_template() -> String {
    omnish_llm::template::prompt_template(true).to_string()
}

const COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        path: "/context",
        kind: CommandKind::Daemon("context"),
        help: "Show LLM context",
    },
    CommandEntry {
        path: "/template",
        kind: CommandKind::Local(debug_template),
        help: "Show prompt template",
    },
    CommandEntry {
        path: "/debug",
        kind: CommandKind::Local(debug_usage),
        help: "Show debug subcommands",
    },
    CommandEntry {
        path: "/debug client",
        kind: CommandKind::Daemon("client_debug"),
        help: "Show client debug state",
    },
    CommandEntry {
        path: "/sessions",
        kind: CommandKind::Daemon("sessions"),
        help: "List sessions",
    },
];

/// Return all command paths for ghost-text completion.
pub fn completable_commands() -> Vec<String> {
    COMMANDS.iter().map(|e| e.path.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Parse redirect suffix: "some text > /path/to/file" -> ("some text", Some("/path/to/file"))
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
pub fn dispatch(msg: &str) -> ChatAction {
    if !msg.starts_with('/') {
        return ChatAction::LlmQuery(msg.to_string());
    }

    let (cmd_str, redirect) = parse_redirect(msg);
    let redirect = redirect.map(|s| s.to_string());

    // Find the longest matching command path.
    let mut best: Option<&CommandEntry> = None;
    for entry in COMMANDS {
        if cmd_str == entry.path
            || cmd_str.starts_with(&format!("{} ", entry.path))
        {
            if best.map_or(true, |b| entry.path.len() > b.path.len()) {
                best = Some(entry);
            }
        }
    }

    if let Some(entry) = best {
        // Check for unknown subcommands: if the user typed more tokens than
        // the matched path, and no longer path matched, it's an error.
        let remainder = cmd_str[entry.path.len()..].trim();
        if !remainder.is_empty() {
            return ChatAction::Command {
                result: format!("Unknown subcommand: {} {}", entry.path, remainder),
                redirect: None,
            };
        }

        match &entry.kind {
            CommandKind::Local(f) => ChatAction::Command {
                result: f(),
                redirect,
            },
            CommandKind::Daemon(key) => ChatAction::DaemonQuery {
                query: format!("__cmd:{}", key),
                redirect,
            },
        }
    } else {
        // Unknown /command — treat as LLM query.
        ChatAction::LlmQuery(msg.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_redirect() {
        assert_eq!(parse_redirect("/context"), ("/context", None));
        assert_eq!(
            parse_redirect("/context > /tmp/out.txt"),
            ("/context", Some("/tmp/out.txt"))
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
    fn test_context_dispatches_to_daemon() {
        match dispatch("/context") {
            ChatAction::DaemonQuery { query, redirect } => {
                assert_eq!(query, "__cmd:context");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_context_with_redirect() {
        match dispatch("/context > /tmp/ctx.txt") {
            ChatAction::DaemonQuery { query, redirect } => {
                assert_eq!(query, "__cmd:context");
                assert_eq!(redirect.as_deref(), Some("/tmp/ctx.txt"));
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_template_is_local() {
        match dispatch("/template") {
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
    fn test_sessions_dispatches_to_daemon() {
        match dispatch("/sessions") {
            ChatAction::DaemonQuery { query, redirect } => {
                assert_eq!(query, "__cmd:sessions");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_unknown_slash_command_falls_through() {
        match dispatch("/unknown foo") {
            ChatAction::LlmQuery(q) => assert_eq!(q, "/unknown foo"),
            _ => panic!("expected LlmQuery"),
        }
    }

    #[test]
    fn test_unknown_debug_subcommand_is_error() {
        match dispatch("/debug bogus") {
            ChatAction::Command { result, redirect } => {
                assert!(result.contains("Unknown subcommand"));
                assert!(result.contains("bogus"));
                assert!(redirect.is_none());
            }
            _ => panic!("expected Command with error"),
        }
    }

    #[test]
    fn test_completable_commands_returns_all_entries() {
        let cmds = completable_commands();
        assert!(cmds.contains(&"/context".to_string()));
        assert!(cmds.contains(&"/template".to_string()));
        assert!(cmds.contains(&"/debug".to_string()));
        assert!(cmds.contains(&"/debug client".to_string()));
        assert!(cmds.contains(&"/sessions".to_string()));
        assert_eq!(cmds.len(), COMMANDS.len());
    }

    #[test]
    fn test_debug_client_dispatches_to_daemon() {
        match dispatch("/debug client") {
            ChatAction::DaemonQuery { query, redirect } => {
                assert_eq!(query, "__cmd:client_debug");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }
}
