/// Result of parsing a chat message for `/` commands.
pub enum ChatAction {
    /// A `/` command was recognized. Contains the result text, optional redirect path,
    /// and optional output limit (head/tail).
    Command {
        result: String,
        redirect: Option<String>,
        limit: Option<OutputLimit>,
    },
    /// Not a command - forward as normal LLM query.
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
    /// Client-side command - handler receives the remainder after the command path.
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
    crate::i18n::tf("command.usage_debug", &[("subs", &subs.join("|"))])
}

fn thread_usage(_args: &str) -> String {
    let mut output = crate::i18n::t("command.usage_thread").to_string();
    for entry in COMMANDS {
        if entry.path.starts_with("/thread ") && !entry.help.is_empty() {
            let sub = &entry.path["/thread ".len()..];
            output.push_str(&format!("  {} - {}\n", sub, help_for(entry)));
        }
    }
    output.push_str(&format!(
        "  sandbox [on|off] - {}\n",
        crate::i18n::t("command.help.thread_sandbox")
    ));
    output.push_str(&format!(
        "  rename [<name>] - {}\n",
        crate::i18n::t("command.help.thread_rename")
    ));
    output
}

// Resolve a CommandEntry's help text via i18n. Key derives from the path:
// "/debug events" -> "command.help.debug_events". Falls back to the English
// `help` field when no translation is registered (e.g. during tests or for
// locales that haven't been fully populated).
fn help_for(entry: &CommandEntry) -> String {
    let slug = entry.path.trim_start_matches('/').replace(' ', "_");
    let key = format!("command.help.{}", slug);
    let translated = crate::i18n::t(&key);
    if translated == key {
        entry.help.to_string()
    } else {
        translated.to_string()
    }
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
        return format!("{}\n\
                \n  tmux   - inject default-shell into ~/.tmux.conf\
                \n  screen - inject shell setting into ~/.screenrc\
                \n  ssh    - show SSH config snippet for RemoteCommand",
                crate::i18n::t("command.usage_integrate"));
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
            // Show ~/.omnish... rather than the local user's expanded HOME,
            // since the snippet runs on the remote host where ~ resolves
            // to a different user's home.
            let home = std::env::var("HOME").unwrap_or_default();
            let display_bin = if !home.is_empty() && omnish_bin.starts_with(&format!("{}/", home)) {
                format!("~{}", &omnish_bin[home.len()..])
            } else {
                omnish_bin.clone()
            };
            format!(
                "Add to ~/.ssh/config for hosts with omnish installed:\n\n\
                 Host <hostname>\n    \
                 RequestTTY yes\n    \
                 RemoteCommand {}\n",
                display_bin
            )
        }
        other => format!("Unknown target: {}. Use tmux, screen, or ssh.", other),
    }
}

fn help_command(_args: &str) -> String {
    let mut output = format!("{}\n", crate::i18n::t("command.help_header"));
    for entry in COMMANDS {
        if entry.help.is_empty() {
            continue;
        }
        // Hide /debug subcommands - /debug itself shows them via its usage handler.
        if entry.path.starts_with("/debug ") {
            continue;
        }
        output.push_str(&format!("  {} - {}\n", entry.path, help_for(entry)));
        // Show /thread subcommands inline under /thread.
        if entry.path == "/thread" {
            for sub in COMMANDS {
                if sub.path.starts_with("/thread ") && !sub.help.is_empty() {
                    output.push_str(&format!("    {} - {}\n", sub.path, help_for(sub)));
                }
            }
            output.push_str(&format!(
                "    /thread sandbox [on|off] - {}\n",
                crate::i18n::t("command.help.thread_sandbox")
            ));
            output.push_str(&format!(
                "    /thread rename [<name>] - {}\n",
                crate::i18n::t("command.help.thread_rename")
            ));
        }
    }
    // Chat-mode-only commands not in the registry.
    output.push_str(&format!("  /resume - {}\n", crate::i18n::t("command.help.resume")));
    output.push_str(&format!("  /model - {}\n", crate::i18n::t("command.help.model")));
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
        path: "/thread",
        kind: CommandKind::Local(thread_usage),
        help: "Manage conversation threads",
    },
    CommandEntry {
        path: "/thread list",
        kind: CommandKind::Daemon("conversations"),
        help: "List recent threads (default 20, /thread list N for more)",
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
    // /update is handled client-side (exec_update needs proxy fd/pid).
    // Hidden from /help - auto-update handles this transparently.
    CommandEntry {
        path: "/update",
        kind: CommandKind::Daemon("update"),
        help: "",
    },
];

/// Chat-mode-only commands (not in COMMANDS registry).
pub const CHAT_ONLY_COMMANDS: &[&str] = &[
    "/resume",
    "/resume all",
    "/model",
    "/thread sandbox",
    "/thread sandbox on",
    "/thread sandbox off",
    "/thread rename",
    "/test lock",
    "/test disconnect",
];

/// Priority order for ghost-text completion (first match wins).
const COMPLETION_PRIORITY: &[&str] = &[
    "/config",
    "/help",
    "/resume",
    "/model",
    "/thread",
    "/debug",
    "/test",
    "/context",
    "/template",
];

/// Return all command paths for ghost-text completion, ordered by priority.
pub fn completable_commands() -> Vec<String> {
    // Collect all raw commands first.
    let mut all: Vec<String> = COMMANDS.iter().map(|e| e.path.to_string()).collect();
    for name in omnish_llm::template::TEMPLATE_NAMES {
        all.push(format!("/template {}", name));
        all.push(format!("/context {}", name));
    }
    for sub in &["tmux", "screen", "ssh"] {
        all.push(format!("/integrate {}", sub));
    }
    for cmd in CHAT_ONLY_COMMANDS {
        all.push(cmd.to_string());
    }

    // Re-order by priority: commands matching each priority prefix come first,
    // preserving relative order within each group.
    let mut result = Vec::with_capacity(all.len());
    for prefix in COMPLETION_PRIORITY {
        // Exact match first (e.g. "/config" before "/config foo").
        if let Some(pos) = all.iter().position(|c| c.as_str() == *prefix) {
            result.push(all.remove(pos));
        }
        // Then subcommands (e.g. "/thread list", "/debug events").
        let mut i = 0;
        while i < all.len() {
            if all[i].starts_with(prefix) && all[i].as_bytes().get(prefix.len()) == Some(&b' ') {
                result.push(all.remove(i));
            } else {
                i += 1;
            }
        }
    }
    // Append remaining commands not covered by priority list.
    result.append(&mut all);
    result
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
        // Unknown /command - treat as LLM query.
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

    #[test]
    fn test_thread_no_args_shows_usage() {
        match dispatch("/thread") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("Usage"));
                assert!(result.contains("list"));
                assert!(result.contains("stats"));
                assert!(result.contains("del"));
                assert!(result.contains("sandbox"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_help_hides_debug_subcommands() {
        match dispatch("/help") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("/debug"));
                assert!(!result.contains("/debug events"));
                assert!(!result.contains("/debug client"));
                assert!(!result.contains("/debug session"));
                assert!(!result.contains("/debug daemon"));
                assert!(!result.contains("/debug commands"));
                assert!(!result.contains("/debug command"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_help_shows_thread_subcommands() {
        match dispatch("/help") {
            ChatAction::Command { result, .. } => {
                assert!(result.contains("/thread"));
                assert!(result.contains("/thread list"));
                assert!(result.contains("/thread stats"));
                assert!(result.contains("/thread del"));
                assert!(result.contains("/thread sandbox"));
                assert!(result.contains("/thread rename"));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn test_completable_commands_priority_order() {
        let cmds = completable_commands();
        let pos = |s: &str| cmds.iter().position(|c| c == s).unwrap();
        assert!(pos("/config") < pos("/help"));
        assert!(pos("/help") < pos("/resume"));
        assert!(pos("/resume") < pos("/model"));
        assert!(pos("/model") < pos("/thread list"));
        assert!(pos("/thread list") < pos("/debug"));
        assert!(pos("/debug") < pos("/test lock"));
        assert!(pos("/test lock") < pos("/context"));
        assert!(pos("/context") < pos("/template"));
    }
}
