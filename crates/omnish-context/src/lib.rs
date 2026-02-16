pub mod recent;

use anyhow::Result;
use async_trait::async_trait;
use omnish_store::command::CommandRecord;
use omnish_store::stream::StreamEntry;

/// Pre-processed command data, ready for formatting.
pub struct CommandContext {
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
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
pub trait ContextFormatter: Send + Sync {
    fn format(&self, commands: &[CommandContext]) -> String;
}

/// Orchestrates: strategy selects commands, reads stream data, formatter produces text.
pub async fn build_context(
    strategy: &dyn ContextStrategy,
    formatter: &dyn ContextFormatter,
    commands: &[CommandRecord],
    reader: &dyn StreamReader,
) -> Result<String> {
    let selected = strategy.select_commands(commands).await;

    let mut contexts = Vec::new();
    for cmd in selected {
        let entries = reader.read_command_output(cmd.stream_offset, cmd.stream_length)?;

        let mut raw_bytes = Vec::new();
        for entry in &entries {
            if entry.direction == 1 {
                raw_bytes.extend_from_slice(&entry.data);
            }
        }

        let output = strip_ansi(&raw_bytes);

        contexts.push(CommandContext {
            command_line: cmd.command_line.clone(),
            cwd: cmd.cwd.clone(),
            started_at: cmd.started_at,
            ended_at: cmd.ended_at,
            output,
        });
    }

    Ok(formatter.format(&contexts))
}

/// Strip ANSI escape sequences from raw bytes.
pub fn strip_ansi(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}
