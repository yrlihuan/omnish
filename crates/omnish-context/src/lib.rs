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
///
/// When `current_session_id` is provided with `min_current_session_detailed > 0`,
/// the split is adjusted so that at least that many commands from the current session
/// appear in the detailed portion (with full output), even if they are older than
/// other sessions' commands.
pub async fn build_context(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
    session_hostnames: &HashMap<String, String>,
    detailed_count: usize,
    max_line_width: usize,
) -> Result<String> {
    build_context_with_session(
        strategy, formatter, commands, reader, session_hostnames,
        detailed_count, max_line_width, None, 0,
    ).await
}

/// Like `build_context` but ensures at least `min_current_session_detailed` commands
/// from the current session appear in the detailed (full output) portion.
pub async fn build_context_with_session(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
    session_hostnames: &HashMap<String, String>,
    detailed_count: usize,
    max_line_width: usize,
    current_session_id: Option<&str>,
    min_current_session_detailed: usize,
) -> Result<String> {
    let selected = strategy.select_commands(commands).await;

    // Determine which commands are detailed vs history.
    // Start with the standard split: last `detailed_count` are detailed.
    let initial_split = selected.len().saturating_sub(detailed_count);
    let (history_cmds, detailed_cmds) = split_with_current_session_minimum(
        &selected, initial_split, current_session_id, min_current_session_detailed,
    );

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

/// Given selected commands and an initial split point, adjust the split so that
/// at least `min_current` commands from `current_session_id` are in the detailed
/// (right) portion. Commands promoted from history to detailed are the most recent
/// ones from the current session that aren't already in detailed.
///
/// Returns (history, detailed) slices as owned Vecs to allow reordering.
pub fn split_with_current_session_minimum<'a>(
    selected: &[&'a CommandRecord],
    initial_split: usize,
    current_session_id: Option<&str>,
    min_current: usize,
) -> (Vec<&'a CommandRecord>, Vec<&'a CommandRecord>) {
    let history_part = &selected[..initial_split];
    let detailed_part = &selected[initial_split..];

    let session_id = match current_session_id {
        Some(id) if min_current > 0 => id,
        _ => return (history_part.to_vec(), detailed_part.to_vec()),
    };

    // Count current session commands already in detailed
    let current_in_detailed = detailed_part.iter()
        .filter(|c| c.session_id == session_id)
        .count();

    if current_in_detailed >= min_current {
        return (history_part.to_vec(), detailed_part.to_vec());
    }

    let needed = min_current - current_in_detailed;

    // Find current session commands in history (most recent first = reverse order)
    let mut promote_indices: Vec<usize> = Vec::new();
    for (i, cmd) in history_part.iter().enumerate().rev() {
        if cmd.session_id == session_id {
            promote_indices.push(i);
            if promote_indices.len() >= needed {
                break;
            }
        }
    }

    if promote_indices.is_empty() {
        return (history_part.to_vec(), detailed_part.to_vec());
    }

    // Build new history (excluding promoted) and new detailed (promoted + original)
    let promote_set: std::collections::HashSet<usize> = promote_indices.iter().copied().collect();
    let mut new_history: Vec<&'a CommandRecord> = history_part.iter()
        .enumerate()
        .filter(|(i, _)| !promote_set.contains(i))
        .map(|(_, c)| *c)
        .collect();
    let mut promoted: Vec<&'a CommandRecord> = promote_indices.iter().rev()
        .map(|&i| history_part[i])
        .collect();
    promoted.extend_from_slice(detailed_part);

    // Sort both by started_at to maintain chronological order
    new_history.sort_by_key(|c| c.started_at);
    promoted.sort_by_key(|c| c.started_at);

    (new_history, promoted)
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
