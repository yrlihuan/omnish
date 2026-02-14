//! omnish-commands: list recent commands from stored sessions.
//!
//! Usage:
//!   omnish-commands           # show last 20 commands across all sessions
//!   omnish-commands -n 50     # show last 50
//!   omnish-commands -s abc123 # filter by session ID prefix

use anyhow::Result;
use omnish_store::command::CommandRecord;
use omnish_store::session::SessionMeta;
use std::path::PathBuf;

struct DisplayCommand {
    record: CommandRecord,
    session_ended: Option<String>,
    parent_session_id: Option<String>,
}

fn store_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("omnish/sessions")
}

fn load_all_commands(base: &PathBuf, session_filter: Option<&str>) -> Result<Vec<DisplayCommand>> {
    let mut all = Vec::new();

    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => {
            eprintln!("No sessions directory at {}", base.display());
            return Ok(all);
        }
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        let meta = match SessionMeta::load(&dir) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if let Some(filter) = session_filter {
            if !meta.session_id.starts_with(filter) {
                continue;
            }
        }

        let commands = CommandRecord::load_all(&dir).unwrap_or_default();
        for record in commands {
            all.push(DisplayCommand {
                record,
                session_ended: meta.ended_at.clone(),
                parent_session_id: meta.parent_session_id.clone(),
            });
        }
    }

    all.sort_by_key(|c| c.record.started_at);
    Ok(all)
}

fn format_timestamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let dt = chrono::DateTime::from_timestamp(secs, 0);
    match dt {
        Some(d) => d.format("%H:%M:%S").to_string(),
        None => format!("{}ms", ms),
    }
}

fn truncate_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn print_commands(commands: &[DisplayCommand], limit: usize) {
    let start = commands.len().saturating_sub(limit);
    let slice = &commands[start..];

    if slice.is_empty() {
        println!("No commands found.");
        return;
    }

    // Header
    println!(
        "{:<12} {:<10} {:<30} {}",
        "SESSION", "TIME", "COMMAND", "OUTPUT (summary)"
    );
    println!("{}", "─".repeat(90));

    for cmd in slice {
        let session = &cmd.record.session_id;
        let time = format_timestamp(cmd.record.started_at);
        let command_line = cmd
            .record
            .command_line
            .as_deref()
            .unwrap_or("(unknown)");
        let nested = if cmd.parent_session_id.is_some() {
            " [N]"
        } else {
            ""
        };
        let status = if cmd.session_ended.is_some() {
            ""
        } else {
            " *"
        };

        println!(
            "{:<12} {:<10} {:<30} {}",
            format!("{}{}{}", session, status, nested),
            time,
            truncate_line(command_line, 28),
            truncate_summary(&cmd.record.output_summary, 40),
        );
    }

    println!("{}", "─".repeat(90));
    println!(
        "{} commands shown (of {} total). * = active session, [N] = nested",
        slice.len(),
        commands.len()
    );
}

fn truncate_summary(summary: &str, max_len: usize) -> String {
    // Take first non-empty line, truncate
    let first_line = summary
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    truncate_line(first_line.trim(), max_len)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut limit: usize = 20;
    let mut session_filter: Option<String> = None;
    let mut show_all = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                i += 1;
                if i < args.len() {
                    limit = args[i].parse().unwrap_or(20);
                }
            }
            "-s" => {
                i += 1;
                if i < args.len() {
                    session_filter = Some(args[i].clone());
                }
            }
            "--all" | "-a" => {
                show_all = true;
            }
            "-h" | "--help" => {
                println!("omnish-commands: list recent commands from stored sessions");
                println!();
                println!("Usage:");
                println!("  omnish-commands              # show last 20 commands (leaf sessions only)");
                println!("  omnish-commands -a           # show all sessions including nested");
                println!("  omnish-commands -n 50        # show last 50");
                println!("  omnish-commands -s abc123    # filter by session ID prefix");
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
            }
        }
        i += 1;
    }

    let base = store_dir();
    let commands = load_all_commands(&base, session_filter.as_deref())?;

    let commands = if show_all {
        commands
    } else {
        // Collect session IDs that are parents of other sessions
        let parent_ids: std::collections::HashSet<String> = commands
            .iter()
            .filter_map(|c| c.parent_session_id.clone())
            .collect();
        // Keep only commands from sessions that are NOT parents
        commands
            .into_iter()
            .filter(|c| !parent_ids.contains(&c.record.session_id))
            .collect()
    };

    print_commands(&commands, limit);

    Ok(())
}
