// crates/omnish-client/src/commands.rs

#[derive(Debug)]
pub enum OmnishCommand {
    Ask { flags: AskFlags, query: String },
    Sessions,
    Status,
    Pause,
    Resume,
    Config { key: String, value: String },
    Replay { session_id: String },
    Unknown(String),
}

#[derive(Debug, Default)]
pub struct AskFlags {
    pub all_sessions: bool,
    pub session_count: Option<usize>,
}

pub fn parse_command(input: &str, prefix: &str) -> Option<OmnishCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let rest = trimmed[prefix.len()..].trim();
    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
    let cmd = parts[0];
    let args = parts.get(1).copied().unwrap_or("");

    Some(match cmd {
        "ask" => {
            let mut flags = AskFlags::default();
            let mut query_parts = Vec::new();
            let mut tokens = args.split_whitespace().peekable();
            while let Some(tok) = tokens.next() {
                match tok {
                    "-a" => flags.all_sessions = true,
                    "-s" => {
                        if let Some(n) = tokens.next() {
                            flags.session_count = n.parse().ok();
                        }
                    }
                    _ => {
                        query_parts.push(tok);
                        query_parts.extend(tokens);
                        break;
                    }
                }
            }
            OmnishCommand::Ask {
                flags,
                query: query_parts.join(" "),
            }
        }
        "sessions" => OmnishCommand::Sessions,
        "status" => OmnishCommand::Status,
        "pause" => OmnishCommand::Pause,
        "resume" => OmnishCommand::Resume,
        "config" => {
            let config_parts: Vec<&str> = args.splitn(2, ' ').collect();
            OmnishCommand::Config {
                key: config_parts.first().unwrap_or(&"").to_string(),
                value: config_parts.get(1).unwrap_or(&"").to_string(),
            }
        }
        "replay" => OmnishCommand::Replay {
            session_id: args.to_string(),
        },
        _ => OmnishCommand::Unknown(rest.to_string()),
    })
}
