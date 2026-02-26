use anyhow::{anyhow, Result};
use omnish_common::config::ContextConfig;
use omnish_context::recent::{GroupedFormatter, RecentCommands};
use omnish_context::StreamReader;
use omnish_store::command::CommandRecord;
use omnish_store::session::SessionMeta;
use omnish_store::stream::{read_range, StreamEntry, StreamWriter};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

struct FileStreamReader {
    stream_path: PathBuf,
}

impl StreamReader for FileStreamReader {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        read_range(&self.stream_path, offset, length)
    }
}

struct MultiSessionReader {
    readers: HashMap<(u64, u64), PathBuf>,
}

impl StreamReader for MultiSessionReader {
    fn read_command_output(&self, offset: u64, length: u64) -> Result<Vec<StreamEntry>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let path = self
            .readers
            .get(&(offset, length))
            .ok_or_else(|| anyhow!("no stream file for offset={}, length={}", offset, length))?;
        read_range(path, offset, length)
    }
}

struct StreamWriterState {
    writer: StreamWriter,
    last_command_stream_pos: u64,
    last_active: Instant,
}

struct Session {
    dir: PathBuf, // immutable after creation
    meta: RwLock<SessionMeta>,
    commands: RwLock<Vec<CommandRecord>>,
    stream_writer: Mutex<StreamWriterState>,
}

pub struct SessionManager {
    base_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    context_config: ContextConfig,
}

/// Infer `last_active` from persisted data so that idle time survives daemon restarts.
/// Falls back to `Instant::now()` when no timestamp is available.
fn infer_last_active(commands: &[CommandRecord], meta: &SessionMeta) -> Instant {
    // Best source: last command's ended_at or started_at (epoch ms).
    let last_cmd_ms = commands
        .last()
        .and_then(|cmd| cmd.ended_at.or(Some(cmd.started_at)));

    // Fallback: session's ended_at or started_at (RFC 3339 string → epoch ms).
    let parse_rfc3339_ms = |s: &str| -> Option<u64> {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp_millis() as u64)
    };

    let ts_ms = last_cmd_ms
        .or_else(|| meta.ended_at.as_deref().and_then(parse_rfc3339_ms))
        .or_else(|| parse_rfc3339_ms(&meta.started_at));

    match ts_ms {
        Some(ms) => {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let age = Duration::from_millis(now_ms.saturating_sub(ms));
            Instant::now() - age
        }
        None => Instant::now(),
    }
}

fn format_idle(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

impl SessionManager {
    pub fn new(base_dir: PathBuf, context_config: ContextConfig) -> Self {
        std::fs::create_dir_all(&base_dir).ok();
        Self {
            base_dir,
            sessions: RwLock::new(HashMap::new()),
            context_config,
        }
    }

    pub async fn load_existing(&self) -> Result<usize> {
        let mut count = 0;
        let entries = match std::fs::read_dir(&self.base_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("failed to read session store directory: {}", e);
                return Ok(0);
            }
        };

        let mut sessions = self.sessions.write().await;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("failed to read directory entry: {}", e);
                    continue;
                }
            };
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }

            let mut load = || -> Result<()> {
                let meta = SessionMeta::load(&dir)?;
                let commands = CommandRecord::load_all(&dir)?;
                let stream_path = dir.join("stream.bin");
                let stream_writer = if stream_path.exists() {
                    StreamWriter::open_append(&stream_path)?
                } else {
                    StreamWriter::create(&stream_path)?
                };

                let last_command_stream_pos = commands
                    .last()
                    .map(|cmd| cmd.stream_offset + cmd.stream_length)
                    .unwrap_or(0);

                let last_active = infer_last_active(&commands, &meta);

                let session_id = meta.session_id.clone();
                sessions.insert(
                    session_id,
                    Arc::new(Session {
                        dir: dir.clone(),
                        meta: RwLock::new(meta),
                        commands: RwLock::new(commands),
                        stream_writer: Mutex::new(StreamWriterState {
                            writer: stream_writer,
                            last_command_stream_pos,
                            last_active,
                        }),
                    }),
                );
                count += 1;
                Ok(())
            };

            if let Err(e) = load() {
                tracing::warn!("removing corrupt session dir {:?}: {}", dir, e);
                if let Err(rm_err) = std::fs::remove_dir_all(&dir) {
                    tracing::error!("failed to remove corrupt session dir {:?}: {}", dir, rm_err);
                }
            }
        }
        Ok(count)
    }

    pub async fn register(
        &self,
        session_id: &str,
        parent_session_id: Option<String>,
        attrs: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        // Fast path: check if session exists with read lock
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(session_id) {
                let mut meta = session.meta.write().await;
                meta.attrs = attrs;
                meta.save(&session.dir)?;
                tracing::info!("session {} re-registered (reconnect)", session_id);
                return Ok(());
            }
        }

        // Slow path: create new session under write lock
        let mut sessions = self.sessions.write().await;

        // Double-check after acquiring write lock
        if let Some(session) = sessions.get(session_id) {
            let mut meta = session.meta.write().await;
            meta.attrs = attrs;
            meta.save(&session.dir)?;
            tracing::info!("session {} re-registered (reconnect)", session_id);
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let session_dir = self
            .base_dir
            .join(format!("{}_{}", now.replace(':', "-"), session_id));
        std::fs::create_dir_all(&session_dir)?;

        let meta = SessionMeta {
            session_id: session_id.to_string(),
            parent_session_id,
            started_at: now,
            ended_at: None,
            attrs,
        };
        meta.save(&session_dir)?;

        let stream_writer = StreamWriter::create(&session_dir.join("stream.bin"))?;

        sessions.insert(
            session_id.to_string(),
            Arc::new(Session {
                dir: session_dir,
                meta: RwLock::new(meta),
                commands: RwLock::new(Vec::new()),
                stream_writer: Mutex::new(StreamWriterState {
                    writer: stream_writer,
                    last_command_stream_pos: 0,
                    last_active: Instant::now(),
                }),
            }),
        );
        Ok(())
    }

    pub async fn write_io(
        &self,
        session_id: &str,
        timestamp_ms: u64,
        direction: u8,
        data: &[u8],
    ) -> Result<()> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let mut sw = session.stream_writer.lock().await;
            sw.writer.write_entry(timestamp_ms, direction, data)?;
            sw.last_active = Instant::now();
        }
        Ok(())
    }

    pub async fn receive_command(&self, session_id: &str, mut record: CommandRecord) -> Result<()> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            // Lock stream_writer to get position info
            let current_pos;
            {
                let mut sw = session.stream_writer.lock().await;
                current_pos = sw.writer.position();
                record.stream_offset = sw.last_command_stream_pos;
                record.stream_length = current_pos - sw.last_command_stream_pos;
                sw.last_command_stream_pos = current_pos;
                sw.last_active = Instant::now();
            }

            // Lock commands to push and save
            let mut commands = session.commands.write().await;
            commands.push(record);
            CommandRecord::save_all(&commands, &session.dir)?;
        }
        Ok(())
    }

    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let mut meta = session.meta.write().await;
            meta.ended_at = Some(chrono::Utc::now().to_rfc3339());
            meta.save(&session.dir)?;

            let commands = session.commands.read().await;
            CommandRecord::save_all(&commands, &session.dir)?;
        }
        Ok(())
    }

    pub async fn get_commands(&self, session_id: &str) -> Result<Vec<CommandRecord>> {
        let session = {
            let sessions = self.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        if let Some(session) = session {
            let commands = session.commands.read().await;
            Ok(commands.clone())
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn list_active(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        let mut result = Vec::new();
        for (_, session) in sessions.iter() {
            let meta = session.meta.read().await;
            if meta.ended_at.is_none() {
                result.push(meta.session_id.clone());
            }
        }
        result
    }

    /// Format a human-readable list of active in-memory sessions.
    ///
    /// Only shows active sessions (ended=false). Output is grouped by host,
    /// with hosts ordered by their most-recently-active session (newest first),
    /// and sessions within each host also ordered newest-first.
    /// The current session is marked with a `*` prefix.
    /// Each host section includes a summary line for dead sessions on that host.
    pub async fn format_sessions_list(&self, current_session_id: &str) -> String {
        // Snapshot data under brief locks
        struct SessionSnapshot {
            session_id: String,
            hostname: Option<String>,
            ended: bool,
            last_active: Instant,
            cmd_count: usize,
            context_cmd_count: usize, // number of detailed commands from this session that would be included in context
        }

        // Collect snapshots and all commands under brief lock
        let (mut snapshots, mut all_commands) = {
            let sessions = self.sessions.read().await;
            let mut snaps = Vec::new();
            let mut all_cmds = Vec::new();
            for session in sessions.values() {
                let commands = session.commands.read().await;
                if commands.is_empty() {
                    continue;
                }
                let meta = session.meta.read().await;
                let sw = session.stream_writer.lock().await;
                // Count only meaningful commands (command_line.is_some())
                let cmd_count = commands.iter().filter(|c| c.command_line.is_some()).count();
                snaps.push(SessionSnapshot {
                    session_id: meta.session_id.clone(),
                    hostname: meta.attrs.get("hostname").cloned(),
                    ended: meta.ended_at.is_some(),
                    last_active: sw.last_active,
                    cmd_count,
                    context_cmd_count: 0, // will be calculated later
                });
                // Only include meaningful commands in all_commands
                all_cmds.extend(commands.iter().filter(|c| c.command_line.is_some()).cloned());
            }
            (snaps, all_cmds)
        };

        if snapshots.is_empty() {
            return "(no sessions)".to_string();
        }

        // Calculate how many detailed commands from each session would be included in context
        if !all_commands.is_empty() {
            // Sort commands by started_at (chronological order)
            all_commands.sort_by_key(|c| c.started_at);

            let total = self.context_config.detailed_commands + self.context_config.history_commands;
            let strategy = RecentCommands::new(total)
                .with_current_session(current_session_id, self.context_config.min_current_session_commands);

            // Use the same select+split logic as build_context_with_session
            let (_history_cmds, detailed_cmds) = omnish_context::select_and_split(
                &strategy,
                &all_commands,
                self.context_config.detailed_commands,
                Some(current_session_id),
                self.context_config.min_current_session_commands,
            ).await;

            // Count detailed commands per session
            let mut detailed_counts: HashMap<String, usize> = HashMap::new();
            for cmd in detailed_cmds {
                *detailed_counts.entry(cmd.session_id.clone()).or_insert(0) += 1;
            }

            // Update context_cmd_count in snapshots (now represents detailed commands only)
            for snapshot in &mut snapshots {
                if let Some(count) = detailed_counts.get(&snapshot.session_id) {
                    snapshot.context_cmd_count = *count;
                }
                // else remains 0
            }
        }

        let mut entries = snapshots;
        entries.sort_by(|a, b| b.last_active.cmp(&a.last_active));

        // Group by host and separate active/dead sessions
        let mut host_sessions: HashMap<String, Vec<&SessionSnapshot>> = HashMap::new();
        let mut dead_stats_by_host: HashMap<String, (usize, usize)> = HashMap::new(); // (session_count, total_commands)

        for s in &entries {
            let host = s.hostname.as_deref().unwrap_or("?").to_string();

            if s.ended {
                // Count dead sessions for statistics
                let stats = dead_stats_by_host.entry(host.clone()).or_insert((0, 0));
                stats.0 += 1; // session count
                stats.1 += s.cmd_count; // total commands
            } else {
                // Add active sessions to display
                host_sessions.entry(host).or_default().push(s);
            }
        }

        // Determine host order based on most recent active session
        let mut host_info: Vec<(String, Vec<&SessionSnapshot>, (usize, usize))> = Vec::new();
        for (host, active_sessions) in host_sessions {
            let dead_stats = dead_stats_by_host.get(&host).copied().unwrap_or((0, 0));
            host_info.push((host, active_sessions, dead_stats));
        }

        // Sort by most recent active session within each host
        host_info.sort_by(|a, b| {
            let latest_a = a.1.first().map(|s| s.last_active).unwrap_or(Instant::now() - Duration::from_secs(3600));
            let latest_b = b.1.first().map(|s| s.last_active).unwrap_or(Instant::now() - Duration::from_secs(3600));
            latest_b.cmp(&latest_a) // newest first
        });

        let mut lines = Vec::new();
        for (host, active_sessions, dead_stats) in host_info {
            lines.push(format!("[{}]", host));

            // Display active sessions (sorted newest first within host)
            for s in active_sessions {
                let idle = format_idle(s.last_active.elapsed().as_secs());
                let is_current = s.session_id == current_session_id;
                let marker = if is_current { "*" } else { " " };
                let (color_start, color_end) = if is_current {
                    ("", "")
                } else {
                    ("\x1b[2m", "\x1b[0m")
                };
                lines.push(format!(
                    "  {}{} {} [active] cmds={}/{} idle={}{}",
                    color_start, marker, s.session_id, s.context_cmd_count, s.cmd_count, idle, color_end,
                ));
            }

            // Add dead sessions summary for this host
            let (dead_sessions, dead_commands) = dead_stats;
            if dead_sessions > 0 {
                lines.push(format!(
                    "  {} dead session(s), {} command(s)",
                    dead_sessions, dead_commands
                ));
            }
        }
        lines.join("\n")
    }

    /// Remove sessions that have been inactive longer than `max_inactive`.
    /// Data is already persisted on disk; evicted sessions will be reloaded
    /// on demand if they reconnect via `register()`.
    pub async fn evict_inactive(&self, max_inactive: std::time::Duration) -> usize {
        let mut sessions = self.sessions.write().await;
        let before = sessions.len();

        let mut to_remove = Vec::new();
        for (sid, session) in sessions.iter() {
            let sw = session.stream_writer.lock().await;
            if sw.last_active.elapsed() >= max_inactive {
                to_remove.push(sid.clone());
            }
        }
        for sid in &to_remove {
            sessions.remove(sid);
        }

        let evicted = before - sessions.len();
        if evicted > 0 {
            tracing::info!("evicted {} inactive session(s) from memory", evicted);
        }
        evicted
    }

    pub async fn get_session_context(&self, session_id: &str) -> Result<String> {
        // Clone data under brief locks
        let (commands, stream_path, hostnames) = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

            let cmds = session.commands.read().await.clone();
            let path = session.dir.join("stream.bin");
            let meta = session.meta.read().await;
            let mut hostnames = HashMap::new();
            if let Some(h) = meta.attrs.get("hostname") {
                hostnames.insert(session_id.to_string(), h.clone());
            }
            (cmds, path, hostnames)
        };

        // Build context outside all locks — expensive I/O happens here
        let reader = FileStreamReader { stream_path };
        let cc = &self.context_config;
        let total = cc.detailed_commands + cc.history_commands;
        let strategy = RecentCommands::new(total)
            .with_current_session(session_id, cc.min_current_session_commands);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let formatter = GroupedFormatter::new(session_id, now_ms, cc.head_lines, cc.tail_lines);
        omnish_context::build_context_with_session(
            &strategy,
            &formatter,
            &commands,
            &reader,
            &hostnames,
            cc.detailed_commands,
            cc.max_line_width,
            Some(session_id),
            cc.min_current_session_commands,
        )
        .await
    }

    /// Collect commands from all sessions where `started_at >= since_ms`.
    /// Returns `(hostname, CommandRecord)` pairs sorted by `started_at`.
    pub async fn collect_recent_commands(&self, since_ms: u64) -> Vec<(String, CommandRecord)> {
        let sessions = self.sessions.read().await;
        let mut result = Vec::new();
        for session in sessions.values() {
            let meta = session.meta.read().await;
            let hostname = meta.attrs.get("hostname").cloned().unwrap_or_default();
            let commands = session.commands.read().await;
            for cmd in commands.iter() {
                if cmd.started_at >= since_ms {
                    result.push((hostname.clone(), cmd.clone()));
                }
            }
        }
        result.sort_by_key(|(_, cmd)| cmd.started_at);
        result
    }

    pub async fn get_all_sessions_context(&self, current_session_id: &str) -> Result<String> {
        let cc = &self.context_config;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Clone data under brief locks
        let (all_commands, offset_to_path, hostnames) = {
            let sessions = self.sessions.read().await;
            let mut all_commands = Vec::new();
            let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();
            let mut hostnames: HashMap<String, String> = HashMap::new();

            for (sid, session) in sessions.iter() {
                let stream_path = session.dir.join("stream.bin");
                let meta = session.meta.read().await;
                if let Some(h) = meta.attrs.get("hostname") {
                    hostnames.insert(sid.clone(), h.clone());
                }
                let commands = session.commands.read().await;
                for cmd in commands.iter() {
                    offset_to_path
                        .insert((cmd.stream_offset, cmd.stream_length), stream_path.clone());
                }
                all_commands.extend(commands.clone());
            }
            (all_commands, offset_to_path, hostnames)
        };

        let mut all_commands = all_commands;
        all_commands.sort_by_key(|c| c.started_at);

        if all_commands.is_empty() {
            return Ok(String::new());
        }

        // Build context outside all locks
        let reader = MultiSessionReader {
            readers: offset_to_path,
        };
        let total = cc.detailed_commands + cc.history_commands;
        let strategy = RecentCommands::new(total)
            .with_current_session(current_session_id, cc.min_current_session_commands);
        let formatter =
            GroupedFormatter::new(current_session_id, now_ms, cc.head_lines, cc.tail_lines);
        omnish_context::build_context_with_session(
            &strategy,
            &formatter,
            &all_commands,
            &reader,
            &hostnames,
            cc.detailed_commands,
            cc.max_line_width,
            Some(current_session_id),
            cc.min_current_session_commands,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_existing_restores_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        // Register a session and add a command via normal flow
        {
            let mgr = SessionManager::new(base.clone(), Default::default());
            mgr.register("sess1", None, Default::default())
                .await
                .unwrap();
            mgr.write_io("sess1", 100, 0, b"$ ls\n").await.unwrap();
            mgr.write_io("sess1", 200, 1, b"file.txt\n").await.unwrap();
            mgr.receive_command(
                "sess1",
                CommandRecord {
                    command_id: "cmd1".into(),
                    session_id: "sess1".into(),
                    command_line: Some("ls".into()),
                    cwd: Some("/tmp".into()),
                    started_at: 100,
                    ended_at: Some(200),
                    output_summary: "file.txt".into(),
                    stream_offset: 0,
                    stream_length: 0,
                    exit_code: None,
                },
            )
            .await
            .unwrap();
            // Drop the manager (simulates daemon shutdown)
        }

        // Create a new manager on the same directory and load existing sessions
        let mgr2 = SessionManager::new(base, Default::default());
        let count = mgr2.load_existing().await.unwrap();
        assert_eq!(count, 1);

        let commands = mgr2.get_commands("sess1").await.unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line.as_deref(), Some("ls"));

        let active = mgr2.list_active().await;
        assert!(active.contains(&"sess1".to_string()));
    }

    #[tokio::test]
    async fn test_format_sessions_list_shows_only_active_with_dead_stats() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base, Default::default());

        // Register three sessions with commands, all on the same host (default "?")
        mgr.register("active1", None, Default::default())
            .await
            .unwrap();
        mgr.register("active2", None, Default::default())
            .await
            .unwrap();
        mgr.register("dead1", None, Default::default())
            .await
            .unwrap();

        // Add commands to all sessions
        mgr.receive_command(
            "active1",
            CommandRecord {
                command_id: "cmd1".into(),
                session_id: "active1".into(),
                command_line: Some("ls".into()),
                cwd: Some("/tmp".into()),
                started_at: 100,
                ended_at: Some(200),
                output_summary: "".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        )
        .await
        .unwrap();

        mgr.receive_command(
            "active2",
            CommandRecord {
                command_id: "cmd2".into(),
                session_id: "active2".into(),
                command_line: Some("pwd".into()),
                cwd: Some("/home".into()),
                started_at: 300,
                ended_at: Some(400),
                output_summary: "".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        )
        .await
        .unwrap();

        mgr.receive_command(
            "dead1",
            CommandRecord {
                command_id: "cmd3".into(),
                session_id: "dead1".into(),
                command_line: Some("echo dead".into()),
                cwd: Some("/var".into()),
                started_at: 500,
                ended_at: Some(600),
                output_summary: "".into(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: None,
            },
        )
        .await
        .unwrap();

        // End the dead session
        mgr.end_session("dead1").await.unwrap();

        // Test with active1 as current
        let output = mgr.format_sessions_list("active1").await;

        // Should only show active sessions
        assert!(output.contains("* active1"), "Should highlight current active session: {}", output);
        assert!(output.contains("  active2 [active]"), "Should show other active session: {}", output);
        assert!(!output.contains("dead1"), "Should not show dead sessions in list: {}", output);

        // Should show dead session statistics for the host
        assert!(output.contains("1 dead session(s), 1 command(s)"), "Should show dead session stats: {}", output);

        // All sessions should have [active] status, not [ended]
        assert!(!output.contains("[ended]"), "Should not show ended status: {}", output);
        assert!(output.contains("[active]"), "Should show active status: {}", output);
    }

    #[tokio::test]
    async fn test_format_sessions_list_with_multiple_dead_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base, Default::default());

        // Create 1 active and 2 dead sessions, all on the same host
        mgr.register("active1", None, Default::default())
            .await
            .unwrap();
        mgr.register("dead1", None, Default::default())
            .await
            .unwrap();
        mgr.register("dead2", None, Default::default())
            .await
            .unwrap();

        // Add commands
        for (session, cmd_count) in &[("active1", 1), ("dead1", 3), ("dead2", 2)] {
            for i in 0..*cmd_count {
                mgr.receive_command(
                    session,
                    CommandRecord {
                        command_id: format!("cmd{}_{}", session, i),
                        session_id: session.to_string(),
                        command_line: Some(format!("command{}", i)),
                        cwd: Some("/tmp".into()),
                        started_at: 100 + i as u64,
                        ended_at: Some(200 + i as u64),
                        output_summary: "".into(),
                        stream_offset: 0,
                        stream_length: 0,
                        exit_code: None,
                    },
                )
                .await
                .unwrap();
            }
        }

        // End dead sessions
        mgr.end_session("dead1").await.unwrap();
        mgr.end_session("dead2").await.unwrap();

        let output = mgr.format_sessions_list("active1").await;

        // Should show correct dead session statistics
        assert!(output.contains("2 dead session(s), 5 command(s)"),
                "Should aggregate dead session stats: {}", output);
        assert!(output.contains("* active1 [active] cmds=1/1"),
                "Should show active session with command count: {}", output);
    }

    #[tokio::test]
    async fn test_format_sessions_list_demo_output() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let mgr = SessionManager::new(base, Default::default());

        // Create sessions on different hosts
        let mut attrs = std::collections::HashMap::new();
        attrs.insert("hostname".to_string(), "workstation".to_string());
        mgr.register("workstation_active", None, attrs.clone()).await.unwrap();

        attrs.insert("hostname".to_string(), "server".to_string());
        mgr.register("server_active1", None, attrs.clone()).await.unwrap();
        mgr.register("server_active2", None, attrs.clone()).await.unwrap();
        mgr.register("server_dead", None, attrs.clone()).await.unwrap();

        // Add commands
        mgr.receive_command("workstation_active", CommandRecord {
            command_id: "cmd1".into(),
            session_id: "workstation_active".into(),
            command_line: Some("ls -la".into()),
            cwd: Some("/home/user".into()),
            started_at: 1000,
            ended_at: Some(1100),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        mgr.receive_command("server_active1", CommandRecord {
            command_id: "cmd2".into(),
            session_id: "server_active1".into(),
            command_line: Some("ps aux".into()),
            cwd: Some("/var/log".into()),
            started_at: 2000,
            ended_at: Some(2100),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        mgr.receive_command("server_active2", CommandRecord {
            command_id: "cmd3".into(),
            session_id: "server_active2".into(),
            command_line: Some("docker ps".into()),
            cwd: Some("/opt/app".into()),
            started_at: 3000,
            ended_at: Some(3100),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        mgr.receive_command("server_dead", CommandRecord {
            command_id: "cmd4".into(),
            session_id: "server_dead".into(),
            command_line: Some("old command".into()),
            cwd: Some("/tmp".into()),
            started_at: 500,
            ended_at: Some(600),
            output_summary: "".into(),
            stream_offset: 0,
            stream_length: 0,
            exit_code: Some(0),
        }).await.unwrap();

        // End the dead session
        mgr.end_session("server_dead").await.unwrap();

        // Show sessions list
        let output = mgr.format_sessions_list("workstation_active").await;

        // Print the output for demonstration
        println!("=== Demo Sessions Output ===");
        println!("{}", output);
        println!("===========================");

        // Verify basic structure
        assert!(output.contains("[workstation]"));
        assert!(output.contains("[server]"));
        assert!(output.contains("* workstation_active [active]"));
        assert!(output.contains("  server_active1 [active]"));
        assert!(output.contains("  server_active2 [active]"));
        assert!(!output.contains("server_dead")); // Should not show dead sessions
        assert!(output.contains("1 dead session(s), 1 command(s)")); // Should show dead stats
    }
}
