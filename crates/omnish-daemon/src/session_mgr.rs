use anyhow::{anyhow, Result};
use omnish_common::config::ContextConfig;
use omnish_context::StreamReader;
use omnish_context::recent::{RecentCommands, GroupedFormatter};
use omnish_store::command::CommandRecord;
use omnish_store::session::SessionMeta;
use omnish_store::stream::{read_range, StreamEntry, StreamWriter};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

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
        let path = self.readers.get(&(offset, length))
            .ok_or_else(|| anyhow!("no stream file for offset={}, length={}", offset, length))?;
        read_range(path, offset, length)
    }
}

struct ActiveSession {
    meta: SessionMeta,
    stream_writer: StreamWriter,
    commands: Vec<CommandRecord>,
    dir: PathBuf,
    /// Stream position at the end of the last completed command.
    /// Used to fill in stream_offset/stream_length for incoming CommandComplete records.
    last_command_stream_pos: u64,
    /// Last time this session received any activity (IO, command, etc.)
    last_active: Instant,
}

pub struct SessionManager {
    base_dir: PathBuf,
    sessions: Mutex<HashMap<String, ActiveSession>>,
    context_config: ContextConfig,
}

/// Infer `last_active` from persisted data so that idle time survives daemon restarts.
/// Falls back to `Instant::now()` when no timestamp is available.
fn infer_last_active(commands: &[CommandRecord], meta: &SessionMeta) -> Instant {
    // Best source: last command's ended_at or started_at (epoch ms).
    let last_cmd_ms = commands.last()
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
            sessions: Mutex::new(HashMap::new()),
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

        let mut sessions = self.sessions.lock().await;
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

                let last_command_stream_pos = commands.last()
                    .map(|cmd| cmd.stream_offset + cmd.stream_length)
                    .unwrap_or(0);

                let last_active = infer_last_active(&commands, &meta);

                sessions.insert(
                    meta.session_id.clone(),
                    ActiveSession {
                        meta,
                        stream_writer,
                        commands,
                        dir: dir.clone(),
                        last_command_stream_pos,
                        last_active,
                    },
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
        let mut sessions = self.sessions.lock().await;

        // Idempotent: if session already exists, update attrs and return
        if let Some(session) = sessions.get_mut(session_id) {
            session.meta.attrs = attrs;
            session.meta.save(&session.dir)?;
            tracing::info!("session {} re-registered (reconnect)", session_id);
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let session_dir = self.base_dir.join(format!(
            "{}_{}",
            now.replace(':', "-"),
            session_id
        ));
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
            ActiveSession {
                meta,
                stream_writer,
                commands: Vec::new(),
                dir: session_dir,
                last_command_stream_pos: 0,
                last_active: Instant::now(),
            },
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
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.stream_writer.write_entry(timestamp_ms, direction, data)?;
            session.last_active = Instant::now();
        }
        Ok(())
    }

    pub async fn receive_command(&self, session_id: &str, mut record: CommandRecord) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            // Fill in stream offsets from daemon's stream writer position.
            // Client sends 0 for these since it doesn't write to stream.bin.
            let current_pos = session.stream_writer.position();
            record.stream_offset = session.last_command_stream_pos;
            record.stream_length = current_pos - session.last_command_stream_pos;
            session.last_command_stream_pos = current_pos;

            session.commands.push(record);
            session.last_active = Instant::now();
            CommandRecord::save_all(&session.commands, &session.dir)?;
        }
        Ok(())
    }

    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.meta.ended_at = Some(chrono::Utc::now().to_rfc3339());
            session.meta.save(&session.dir)?;
            CommandRecord::save_all(&session.commands, &session.dir)?;
        }
        Ok(())
    }

    pub async fn get_commands(&self, session_id: &str) -> Result<Vec<CommandRecord>> {
        let sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            Ok(session.commands.clone())
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn list_active(&self) -> Vec<String> {
        let sessions = self.sessions.lock().await;
        sessions.values()
            .filter(|s| s.meta.ended_at.is_none())
            .map(|s| s.meta.session_id.clone())
            .collect()
    }

    /// Format a human-readable list of all in-memory sessions.
    ///
    /// Sessions with zero commands are omitted. Output is grouped by host,
    /// with hosts ordered by their most-recently-active session (newest first),
    /// and sessions within each host also ordered newest-first.
    pub async fn format_sessions_list(&self) -> String {
        let sessions = self.sessions.lock().await;

        // Collect sessions that have at least one command.
        let mut entries: Vec<&ActiveSession> = sessions.values()
            .filter(|s| !s.commands.is_empty())
            .collect();

        if entries.is_empty() {
            return "(no sessions)".to_string();
        }

        // Sort all entries by last_active descending (newest first).
        entries.sort_by(|a, b| b.last_active.cmp(&a.last_active));

        // Group by host, preserving the order of first appearance (= most recent).
        let mut host_order: Vec<&str> = Vec::new();
        let mut by_host: HashMap<&str, Vec<&ActiveSession>> = HashMap::new();
        for s in &entries {
            let host = s.meta.attrs.get("hostname").map(|h| h.as_str()).unwrap_or("?");
            if !by_host.contains_key(host) {
                host_order.push(host);
            }
            by_host.entry(host).or_default().push(s);
        }

        let mut lines = Vec::new();
        for host in host_order {
            lines.push(format!("[{}]", host));
            for s in &by_host[host] {
                let status = if s.meta.ended_at.is_some() { "ended" } else { "active" };
                let idle = format_idle(s.last_active.elapsed().as_secs());
                let cmds = s.commands.len();
                lines.push(format!(
                    "  {} [{}] cmds={} idle={}",
                    s.meta.session_id, status, cmds, idle,
                ));
            }
        }
        lines.join("\n")
    }

    /// Remove sessions that have been inactive longer than `max_inactive`.
    /// Data is already persisted on disk; evicted sessions will be reloaded
    /// on demand if they reconnect via `register()`.
    pub async fn evict_inactive(&self, max_inactive: std::time::Duration) -> usize {
        let mut sessions = self.sessions.lock().await;
        let before = sessions.len();
        sessions.retain(|_sid, session| session.last_active.elapsed() < max_inactive);
        let evicted = before - sessions.len();
        if evicted > 0 {
            tracing::info!("evicted {} inactive session(s) from memory", evicted);
        }
        evicted
    }

    pub async fn get_session_context(&self, session_id: &str) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

        let reader = FileStreamReader {
            stream_path: session.dir.join("stream.bin"),
        };
        let cc = &self.context_config;
        let total = cc.detailed_commands + cc.history_commands;
        let strategy = RecentCommands::new(total);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let formatter = GroupedFormatter::new(session_id, now_ms, cc.head_lines, cc.tail_lines);
        let mut hostnames = HashMap::new();
        if let Some(h) = session.meta.attrs.get("hostname") {
            hostnames.insert(session_id.to_string(), h.clone());
        }
        omnish_context::build_context(&strategy, &formatter, &session.commands, &reader, &hostnames, cc.detailed_commands).await
    }

    pub async fn get_all_sessions_context(&self, current_session_id: &str) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let cc = &self.context_config;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut all_commands = Vec::new();
        let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();
        let mut hostnames: HashMap<String, String> = HashMap::new();

        for (sid, session) in sessions.iter() {
            let stream_path = session.dir.join("stream.bin");
            if let Some(h) = session.meta.attrs.get("hostname") {
                hostnames.insert(sid.clone(), h.clone());
            }
            for cmd in &session.commands {
                offset_to_path.insert(
                    (cmd.stream_offset, cmd.stream_length),
                    stream_path.clone(),
                );
            }
            all_commands.extend(session.commands.clone());
        }

        all_commands.sort_by_key(|c| c.started_at);

        if all_commands.is_empty() {
            return Ok(String::new());
        }

        let reader = MultiSessionReader { readers: offset_to_path };
        let total = cc.detailed_commands + cc.history_commands;
        let strategy = RecentCommands::new(total);
        let formatter = GroupedFormatter::new(current_session_id, now_ms, cc.head_lines, cc.tail_lines);
        omnish_context::build_context(&strategy, &formatter, &all_commands, &reader, &hostnames, cc.detailed_commands).await
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
            mgr.register("sess1", None, Default::default()).await.unwrap();
            mgr.write_io("sess1", 100, 0, b"$ ls\n").await.unwrap();
            mgr.write_io("sess1", 200, 1, b"file.txt\n").await.unwrap();
            mgr.receive_command("sess1", CommandRecord {
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
            }).await.unwrap();
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
}
