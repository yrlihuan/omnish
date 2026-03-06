/// Result of parsing a chat message for `/` commands.
pub enum ChatAction {
    /// A `/` command was recognized. Contains the result text, optional redirect path,
    /// and optional output limit (head/tail).
    Command {
        result: String,
        redirect: Option<String>,
        limit: Option<OutputLimit>,
    },
    /// Not a command — forward as normal LLM query.
    #[allow(dead_code)]
    LlmQuery(String),
    /// A `/` command that needs daemon data. Contains the query to send, optional redirect,
    /// and optional output limit.
    DaemonQuery {
        query: String,
        redirect: Option<String>,
        limit: Option<OutputLimit>,
    },
}

/// Limit applied to command output (head or tail).
#[derive(Clone, Debug)]
pub struct OutputLimit {
    pub kind: OutputLimitKind,
    pub count: usize,
}

#[derive(Clone, Debug)]
pub enum OutputLimitKind {
    Head,
    Tail,
}

// ---------------------------------------------------------------------------
// Command registry
// ---------------------------------------------------------------------------

enum CommandKind {
    /// Client-side command — handler receives the remainder after the command path.
    Local(fn(&str) -> String),
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

fn debug_usage(_args: &str) -> String {
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

fn template_command(args: &str) -> String {
    use omnish_llm::template::{template_by_name, TEMPLATE_NAMES};

    if args.is_empty() {
        return format!(
            "Usage: /template <{}> [> file.txt]",
            TEMPLATE_NAMES.join("|")
        );
    }
    match template_by_name(args) {
        Some(t) => t,
        None => format!(
            "Unknown template: {}\nAvailable: {}",
            args,
            TEMPLATE_NAMES.join(", ")
        ),
    }
}

fn help_command(_args: &str) -> String {
    let mut output = String::from("Available commands:\n");
    for entry in COMMANDS {
        output.push_str(&format!("  {} — {}\n", entry.path, entry.help));
    }
    output
}

fn events_command(args: &str) -> String {
    let n: usize = args.trim().parse().unwrap_or(20);
    let events = crate::event_log::recent(n);
    if events.is_empty() {
        "No events recorded yet.".to_string()
    } else {
        events.join("\n")
    }
}

const COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        path: "/context",
        kind: CommandKind::Daemon("context"),
        help: "Show LLM context or template",
    },
    CommandEntry {
        path: "/template",
        kind: CommandKind::Local(template_command),
        help: "Show prompt template",
    },
    CommandEntry {
        path: "/help",
        kind: CommandKind::Local(help_command),
        help: "Show available commands",
    },
    CommandEntry {
        path: "/debug",
        kind: CommandKind::Local(debug_usage),
        help: "Show debug subcommands",
    },
    CommandEntry {
        path: "/debug events",
        kind: CommandKind::Local(events_command),
        help: "Show recent client events",
    },
    // HACK: registered as Daemon but intercepted client-side in main.rs
    // because it needs local client state (shell_input, interceptor, etc.)
    // that doesn't fit the Local fn(&str) -> String signature.
    CommandEntry {
        path: "/debug client",
        kind: CommandKind::Daemon("client_debug"),
        help: "Show client debug state",
    },
    CommandEntry {
        path: "/debug session",
        kind: CommandKind::Daemon("session"),
        help: "Show current session debug information",
    },
    CommandEntry {
        path: "/sessions",
        kind: CommandKind::Daemon("sessions"),
        help: "List sessions",
    },
    CommandEntry {
        path: "/conversations",
        kind: CommandKind::Daemon("conversations"),
        help: "List all conversations",
    },
    CommandEntry {
        path: "/threads",
        kind: CommandKind::Daemon("conversations"),
        help: "List all conversations (alias for /conversations)",
    },
    CommandEntry {
        path: "/tasks",
        kind: CommandKind::Daemon("tasks"),
        help: "List or manage scheduled tasks",
    },
];

/// Chat-mode-only commands (not in COMMANDS registry).
pub const CHAT_ONLY_COMMANDS: &[&str] = &["/chat", "/ask", "/resume", "/new"];

/// Return all command paths for ghost-text completion.
pub fn completable_commands() -> Vec<String> {
    let mut cmds: Vec<String> = COMMANDS.iter().map(|e| e.path.to_string()).collect();
    for name in omnish_llm::template::TEMPLATE_NAMES {
        cmds.push(format!("/template {}", name));
        cmds.push(format!("/context {}", name));
    }
    for cmd in CHAT_ONLY_COMMANDS {
        cmds.push(cmd.to_string());
    }
    cmds
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

/// Parse head/tail limit suffix: "cmd | head -n 10" -> ("cmd", Some(Head, 10))
/// Supports: | head -n N, | head N, | tail -n N, | tail N
fn parse_limit(input: &str) -> (&str, Option<OutputLimit>) {
    // Find the last occurrence of | head or | tail
    if let Some(pos) = input.find(" | ") {
        let suffix = &input[pos + 3..];
        let base = input[..pos].trim_end();

        let parts: Vec<&str> = suffix.split_whitespace().collect();
        if parts.is_empty() {
            return (input, None);
        }

        let (kind, count) = match parts[0] {
            "head" => {
                // | head or | head N or | head -n N (default to 10 if no N)
                let n = if parts.len() >= 2 {
                    if parts[1] == "-n" {
                        parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(10)
                    } else {
                        parts[1].parse().unwrap_or(10)
                    }
                } else {
                    10
                };
                (OutputLimitKind::Head, n)
            }
            "tail" => {
                // | tail or | tail N or | tail -n N (default to 10 if no N)
                let n = if parts.len() >= 2 {
                    if parts[1] == "-n" {
                        parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(10)
                    } else {
                        parts[1].parse().unwrap_or(10)
                    }
                } else {
                    10
                };
                (OutputLimitKind::Tail, n)
            }
            _ => return (input, None),
        };

        return (base, Some(OutputLimit { kind, count }));
    }
    (input, None)
}

/// Apply output limit to text (head or tail).
pub fn apply_limit(text: &str, limit: &OutputLimit) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let count = limit.count.min(lines.len());

    let selected = match limit.kind {
        OutputLimitKind::Head => &lines[..count],
        OutputLimitKind::Tail => &lines[lines.len() - count..],
    };

    selected.join("\n")
}

/// Dispatch a chat message. Returns ChatAction describing what to do.
pub fn dispatch(msg: &str) -> ChatAction {
    if !msg.starts_with('/') {
        return ChatAction::LlmQuery(msg.to_string());
    }

    // Parse limit (head/tail) first, then redirect
    let (cmd_str_without_limit, limit) = parse_limit(msg);
    let (cmd_str, redirect) = parse_redirect(cmd_str_without_limit);
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
        let remainder = cmd_str[entry.path.len()..].trim();

        match &entry.kind {
            CommandKind::Local(f) => ChatAction::Command {
                result: f(remainder),
                redirect,
                limit,
            },
            CommandKind::Daemon(key) => {
                let query = if remainder.is_empty() {
                    format!("__cmd:{}", key)
                } else {
                    format!("__cmd:{} {}", key, remainder)
                };
                ChatAction::DaemonQuery { query, redirect, limit }
            }
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
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:context");
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_context_with_redirect() {
        match dispatch("/context > /tmp/ctx.txt") {
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:context");
                assert_eq!(redirect.as_deref(), Some("/tmp/ctx.txt"));
                assert!(limit.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_template_no_args_shows_usage() {
        match dispatch("/template") {
            ChatAction::Command { result, redirect, limit } => {
                assert!(result.contains("Usage"));
                assert!(result.contains("chat"));
                assert!(result.contains("auto-complete"));
                assert!(result.contains("daily-notes"));
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_template_chat() {
        match dispatch("/template chat") {
            ChatAction::Command { result, redirect, limit } => {
                assert!(result.contains("{context}"));
                assert!(result.contains("{query}"));
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_template_auto_complete() {
        match dispatch("/template auto-complete") {
            ChatAction::Command { result, redirect, limit } => {
                assert!(result.contains("completion engine"));
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_template_daily_notes() {
        match dispatch("/template daily-notes") {
            ChatAction::Command { result, redirect, limit } => {
                assert!(result.contains("<commands>"));
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_template_unknown_name() {
        match dispatch("/template bogus") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("Unknown template: bogus"));
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
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:sessions");
                assert!(redirect.is_none());
                assert!(limit.is_none());
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
    fn test_unknown_debug_subcommand_shows_usage() {
        // /debug is a Local command that ignores extra args and shows usage.
        match dispatch("/debug bogus") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("Usage"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_completable_commands_returns_all_entries() {
        let cmds = completable_commands();
        assert!(cmds.contains(&"/context".to_string()));
        assert!(cmds.contains(&"/template".to_string()));
        assert!(cmds.contains(&"/help".to_string()));
        assert!(cmds.contains(&"/debug".to_string()));
        assert!(cmds.contains(&"/debug client".to_string()));
        assert!(cmds.contains(&"/debug events".to_string()));
        assert!(cmds.contains(&"/debug session".to_string()));
        assert!(cmds.contains(&"/sessions".to_string()));
        assert!(cmds.contains(&"/conversations".to_string()));
        assert!(cmds.contains(&"/threads".to_string()));
        // Template and context subcommands are also completable.
        assert!(cmds.contains(&"/template chat".to_string()));
        assert!(cmds.contains(&"/template auto-complete".to_string()));
        assert!(cmds.contains(&"/template daily-notes".to_string()));
        assert!(cmds.contains(&"/context chat".to_string()));
        assert!(cmds.contains(&"/context auto-complete".to_string()));
        assert!(cmds.contains(&"/context daily-notes".to_string()));
        assert!(cmds.contains(&"/context hourly-notes".to_string()));
        // Chat-mode commands
        assert!(cmds.contains(&"/chat".to_string()));
        assert!(cmds.contains(&"/ask".to_string()));
        assert!(cmds.contains(&"/resume".to_string()));
        assert!(cmds.contains(&"/new".to_string()));
        // COMMANDS + /template subcommands + /context subcommands + chat commands
        let template_count = omnish_llm::template::TEMPLATE_NAMES.len();
        assert_eq!(
            cmds.len(),
            COMMANDS.len() + template_count * 2 + CHAT_ONLY_COMMANDS.len()
        );
    }

    #[test]
    fn test_tasks_dispatches_to_daemon() {
        match dispatch("/tasks") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:tasks");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_tasks_disable_forwards_args() {
        match dispatch("/tasks disable eviction") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:tasks disable eviction");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_debug_client_dispatches_to_daemon() {
        match dispatch("/debug client") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:client_debug");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_debug_session_dispatches_to_daemon() {
        match dispatch("/debug session") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:session");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_debug_events_command() {
        // Push a test event and verify it appears
        crate::event_log::push("test-event-123");
        match dispatch("/debug events 5") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("test-event-123"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_debug_events_default_count() {
        match dispatch("/debug events") {
            ChatAction::Command { result, .. } => {
                // Should not panic; returns either events or "No events" message
                assert!(!result.is_empty());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_help_command() {
        match dispatch("/help") {
            ChatAction::Command { result, redirect, .. } => {
                assert!(result.contains("Available commands"));
                assert!(result.contains("/context"));
                assert!(result.contains("/help"));
                assert!(redirect.is_none());
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_threads_alias() {
        // /threads should be an alias for /conversations
        match dispatch("/threads") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:conversations");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_conversations_command() {
        match dispatch("/conversations") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:conversations");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }
}
