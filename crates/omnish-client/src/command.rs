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
            &e.path["/debug ".len()..]
        })
        .collect();
    format!("Usage: /debug <{}> [> file.txt]", subs.join("|"))
}

fn integrate_command(args: &str) -> String {
    let omnish_bin = {
        let home = std::env::var("OMNISH_HOME")
            .unwrap_or_else(|_| {
                let h = std::env::var("HOME").unwrap_or_default();
                format!("{}/.omnish", h)
            });
        format!("{}/bin/omnish", home)
    };

    let target = args.trim();
    if target.is_empty() {
        return "Usage: /integrate <tmux|screen|ssh>\n\
                \n  tmux   — inject default-shell into ~/.tmux.conf\
                \n  screen — inject shell setting into ~/.screenrc\
                \n  ssh    — show SSH config snippet for RemoteCommand".to_string();
    }

    match target {
        "tmux" => {
            let home = std::env::var("HOME").unwrap_or_default();
            let conf = format!("{}/.tmux.conf", home);
            let snippet = format!(
                "\n# omnish integration\n\
                 if-shell \"[ -x {} ]\" \\\n    \
                 \"set-option -g default-shell {}\"\n\
                 set-window-option -g allow-rename on\n",
                omnish_bin, omnish_bin
            );

            // Check if already integrated
            if let Ok(content) = std::fs::read_to_string(&conf) {
                if content.contains("omnish integration") {
                    return format!("Already integrated in {}", conf);
                }
            }

            match std::fs::OpenOptions::new().create(true).append(true).open(&conf) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(snippet.as_bytes());
                    format!("Added omnish integration to {}", conf)
                }
                Err(e) => format!("Failed to write {}: {}", conf, e),
            }
        }
        "screen" => {
            let home = std::env::var("HOME").unwrap_or_default();
            let conf = format!("{}/.screenrc", home);
            let snippet = format!(
                "\n# omnish integration\nshell {}\n",
                omnish_bin
            );

            if let Ok(content) = std::fs::read_to_string(&conf) {
                if content.contains("omnish integration") {
                    return format!("Already integrated in {}", conf);
                }
            }

            match std::fs::OpenOptions::new().create(true).append(true).open(&conf) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(snippet.as_bytes());
                    format!("Added omnish integration to {}", conf)
                }
                Err(e) => format!("Failed to write {}: {}", conf, e),
            }
        }
        "ssh" => {
            format!(
                "Add to ~/.ssh/config for hosts with omnish installed:\n\n\
                 Host <hostname>\n    \
                 RequestTTY yes\n    \
                 RemoteCommand {}\n",
                omnish_bin
            )
        }
        other => format!("Unknown target: {}. Use tmux, screen, or ssh.", other),
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
        kind: CommandKind::Daemon("template"),
        help: "Show prompt template",
    },
    CommandEntry {
        path: "/config",
        kind: CommandKind::Daemon("config"),
        help: "Open settings menu (in chat mode)",
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
        path: "/debug daemon",
        kind: CommandKind::Daemon("daemon"),
        help: "Show daemon version, tasks, and auto-update status",
    },
    CommandEntry {
        path: "/debug commands",
        kind: CommandKind::Daemon("commands"),
        help: "Show recent shell commands (default 30)",
    },
    CommandEntry {
        path: "/debug command",
        kind: CommandKind::Daemon("command"),
        help: "Show full details of a command by seq number",
    },
    CommandEntry {
        path: "/sessions",
        kind: CommandKind::Daemon("sessions"),
        help: "List sessions",
    },
    CommandEntry {
        path: "/thread list",
        kind: CommandKind::Daemon("conversations"),
        help: "List all conversation threads",
    },
    CommandEntry {
        path: "/thread stats",
        kind: CommandKind::Daemon("conversations stats"),
        help: "Show token usage statistics for all threads",
    },
    CommandEntry {
        path: "/thread del",
        kind: CommandKind::Daemon("conversations del"),
        help: "Delete a conversation thread",
    },
    CommandEntry {
        path: "/tasks",
        kind: CommandKind::Daemon("tasks"),
        help: "List or manage scheduled tasks",
    },
    // Registered as Daemon but intercepted client-side in main.rs
    // because it needs process state (proxy fd/pid) for exec.
    CommandEntry {
        path: "/integrate",
        kind: CommandKind::Local(integrate_command),
        help: "Integrate omnish with tmux, screen, or ssh",
    },
    CommandEntry {
        path: "/update",
        kind: CommandKind::Daemon("__cmd:update"),
        help: "Re-exec client from updated binary on disk",
    },
];

/// Chat-mode-only commands (not in COMMANDS registry).
pub const CHAT_ONLY_COMMANDS: &[&str] = &["/resume", "/model", "/test lock"];

/// Return all command paths for ghost-text completion.
pub fn completable_commands() -> Vec<String> {
    let mut cmds: Vec<String> = COMMANDS.iter().map(|e| e.path.to_string()).collect();
    for name in omnish_llm::template::TEMPLATE_NAMES {
        cmds.push(format!("/template {}", name));
        cmds.push(format!("/context {}", name));
    }
    // /integrate subcommands
    for sub in &["tmux", "screen", "ssh"] {
        cmds.push(format!("/integrate {}", sub));
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
/// Supports: | head -n N, | head -nN, | head N, | tail -n N, | tail -nN, | tail N
fn parse_limit(input: &str) -> (&str, Option<OutputLimit>) {
    // Find the last occurrence of | head or | tail
    if let Some(pos) = input.find(" | ") {
        let suffix = &input[pos + 3..];
        let base = input[..pos].trim_end();

        let parts: Vec<&str> = suffix.split_whitespace().collect();
        if parts.is_empty() {
            return (input, None);
        }

        // Helper to parse count from -nN or -n N or just N
        let parse_count = |s: &str| -> Option<usize> {
            if let Some(n) = s.strip_prefix("-n") {
                if n.is_empty() {
                    // -n without number: will get next part if available
                    None
                } else {
                    n.parse().ok()
                }
            } else {
                s.parse().ok()
            }
        };

        let (kind, count) = match parts[0] {
            "head" => {
                let n = if parts.len() >= 2 {
                    if parts[1] == "-n" {
                        // | head -n N
                        parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(10)
                    } else {
                        // | head -nN or | head N
                        parse_count(parts[1]).unwrap_or(10)
                    }
                } else {
                    10
                };
                (OutputLimitKind::Head, n)
            }
            "tail" => {
                let n = if parts.len() >= 2 {
                    if parts[1] == "-n" {
                        // | tail -n N
                        parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(10)
                    } else {
                        // | tail -nN or | tail N
                        parse_count(parts[1]).unwrap_or(10)
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

/// Public wrapper for parse_redirect (used by main.rs for /context with redirect).
pub fn parse_redirect_pub(input: &str) -> (&str, Option<&str>) {
    parse_redirect(input)
}

/// Public wrapper for parse_limit (used by main.rs for /context with pipes).
pub fn parse_limit_pub(input: &str) -> (&str, Option<OutputLimit>) {
    parse_limit(input)
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

    // Parse redirect first, then limit
    let (cmd_str_without_redirect, redirect) = parse_redirect(msg);
    let redirect = redirect.map(|s| s.to_string());
    let (cmd_str, limit) = parse_limit(cmd_str_without_redirect);

    // Find the longest matching command path.
    let mut best: Option<&CommandEntry> = None;
    for entry in COMMANDS {
        if (cmd_str == entry.path
            || cmd_str.starts_with(&format!("{} ", entry.path)))
            && best.is_none_or(|b: &CommandEntry| entry.path.len() > b.path.len())
        {
            best = Some(entry);
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
    fn test_context_with_limit_and_redirect() {
        // Both | tail and > redirect
        match dispatch("/context | tail 5 > /tmp/ctx.txt") {
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:context");
                assert_eq!(redirect.as_deref(), Some("/tmp/ctx.txt"));
                assert!(limit.is_some());
                assert_eq!(limit.as_ref().unwrap().count, 5);
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_template_no_args_dispatches_to_daemon() {
        match dispatch("/template") {
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:template");
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_template_chat_dispatches_to_daemon() {
        match dispatch("/template chat") {
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:template chat");
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }

    #[test]
    fn test_template_auto_complete_dispatches_to_daemon() {
        match dispatch("/template auto-complete") {
            ChatAction::DaemonQuery { query, redirect, limit } => {
                assert_eq!(query, "__cmd:template auto-complete");
                assert!(redirect.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("expected DaemonQuery"),
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
        assert!(cmds.contains(&"/thread list".to_string()));
        assert!(cmds.contains(&"/thread del".to_string()));
        // Template and context subcommands are also completable.
        assert!(cmds.contains(&"/template chat".to_string()));
        assert!(cmds.contains(&"/template auto-complete".to_string()));
        assert!(cmds.contains(&"/template daily-notes".to_string()));
        assert!(cmds.contains(&"/context chat".to_string()));
        assert!(cmds.contains(&"/context auto-complete".to_string()));
        assert!(cmds.contains(&"/context daily-notes".to_string()));
        assert!(cmds.contains(&"/context hourly-notes".to_string()));
        // Chat-mode command
        assert!(cmds.contains(&"/resume".to_string()));
        // COMMANDS + /template subcommands + /context subcommands + /integrate subcommands + chat commands
        let template_count = omnish_llm::template::TEMPLATE_NAMES.len();
        let integrate_subs = 3; // tmux, screen, ssh
        assert_eq!(
            cmds.len(),
            COMMANDS.len() + template_count * 2 + integrate_subs + CHAT_ONLY_COMMANDS.len()
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
    fn test_thread_list() {
        // /thread list should dispatch to __cmd:conversations
        match dispatch("/thread list") {
            ChatAction::DaemonQuery { query, redirect, .. } => {
                assert_eq!(query, "__cmd:conversations");
                assert!(redirect.is_none());
            }
            _ => panic!("expected DaemonQuery"),
        }
    }
}
