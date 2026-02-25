pub mod format_utils;
pub mod recent;

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;
use omnish_store::stream::StreamEntry;

/// Pre-processed command data, ready for formatting.
pub struct CommandContext {
    pub session_id: String,
    pub hostname: Option<String>,
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
    pub exit_code: Option<i32>,
}

/// Reads stream entries for a given command's byte range.
pub trait StreamReader: Send + Sync {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>>;
}

/// Selects which commands to include in context.
#[async_trait]
pub trait ContextStrategy: Send + Sync {
    async fn select_commands<'a>(&self, commands: &'a [CommandRecord]) -> Vec<&'a CommandRecord>;
}

/// Formats selected commands into the final context string.
/// `history` contains older commands (command-line only, no output).
/// `detailed` contains recent commands with full output.
pub trait ContextFormatter: Send + Sync {
    fn format(&self, history: &[CommandContext], detailed: &[CommandContext]) -> String;
}

/// Orchestrates: strategy selects commands, reads stream data, formatter produces text.
/// `session_hostnames` maps session_id -> hostname for display in context headers.
/// `detailed_count` controls how many of the most recent selected commands get full output;
/// the rest are treated as history (command-line only).
pub async fn build_context(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
    session_hostnames: &HashMap<String, String>,
    detailed_count: usize,
    max_line_width: usize,
) -> Result<String> {
    let selected = strategy.select_commands(commands).await;
    let split = selected.len().saturating_sub(detailed_count);

    let history_cmds = &selected[..split];
    let detailed_cmds = &selected[split..];

    // History: command-line only, no stream reading
    let history: Vec<CommandContext> = history_cmds
        .iter()
        .map(|cmd| CommandContext {
            session_id: cmd.session_id.clone(),
            hostname: session_hostnames.get(&cmd.session_id).cloned(),
            command_line: cmd.command_line.clone(),
            cwd: cmd.cwd.clone(),
            started_at: cmd.started_at,
            ended_at: cmd.ended_at,
            output: String::new(),
            exit_code: cmd.exit_code,
        })
        .collect();

    // Detailed: full stream reading
    let mut detailed = Vec::new();
    for cmd in detailed_cmds {
        let entries = reader.read_command_output(cmd.stream_offset, cmd.stream_length)?;

        let mut raw_bytes = Vec::new();
        for entry in &entries {
            if entry.direction == 1 {
                raw_bytes.extend_from_slice(&entry.data);
            }
        }

        let output = strip_ansi(&raw_bytes);
        // The PTY output stream starts with the prompt + echoed command line;
        // strip that first line since the command is already shown in the header.
        let output = match output.find('\n') {
            Some(pos) => output[pos + 1..].to_string(),
            None => String::new(),
        };

        // Truncate overly long lines (e.g. snap progress bars).
        let output = format_utils::truncate_line_width(&output, max_line_width);

        detailed.push(CommandContext {
            session_id: cmd.session_id.clone(),
            hostname: session_hostnames.get(&cmd.session_id).cloned(),
            command_line: cmd.command_line.clone(),
            cwd: cmd.cwd.clone(),
            started_at: cmd.started_at,
            ended_at: cmd.ended_at,
            output,
            exit_code: cmd.exit_code,
        });
    }

    Ok(formatter.format(&history, &detailed))
}

/// Strip ANSI escape sequences (CSI and OSC) from raw bytes.
pub fn strip_ansi(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    // CSI: ESC [ ... <alpha>
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    // OSC: ESC ] ... BEL or ESC backslash
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        if next == '\x07' {
                            chars.next();
                            break;
                        }
                        if next == '\x1b' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                _ => {
                    // Other ESC sequence — skip one char
                    chars.next();
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}
