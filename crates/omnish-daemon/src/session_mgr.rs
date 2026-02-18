use anyhow::{anyhow, Result};
use omnish_context::StreamReader;
use omnish_context::recent::{RecentCommands, GroupedFormatter};
use omnish_store::command::CommandRecord;
use omnish_store::session::SessionMeta;
use omnish_store::stream::{read_range, StreamEntry, StreamWriter};
use std::collections::HashMap;
use std::path::PathBuf;
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
}

pub struct SessionManager {
    base_dir: PathBuf,
    sessions: Mutex<HashMap<String, ActiveSession>>,
}

impl SessionManager {
    pub fn new(base_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&base_dir).ok();
        Self {
            base_dir,
            sessions: Mutex::new(HashMap::new()),
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

                sessions.insert(
                    meta.session_id.clone(),
                    ActiveSession {
                        meta,
                        stream_writer,
                        commands,
                        dir: dir.clone(),
                        last_command_stream_pos,
                    },
                );
                count += 1;
                Ok(())
            };

            if let Err(e) = load() {
                tracing::warn!("failed to load session from {:?}: {}", dir, e);
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

    pub async fn get_session_context(&self, session_id: &str) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found: {}", session_id))?;

        let reader = FileStreamReader {
            stream_path: session.dir.join("stream.bin"),
        };
        let strategy = RecentCommands::new();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let formatter = GroupedFormatter::new(session_id, now_ms);
        omnish_context::build_context(&strategy, &formatter, &session.commands, &reader).await
    }

    pub async fn get_all_sessions_context(&self, current_session_id: &str) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut all_commands = Vec::new();
        let mut offset_to_path: HashMap<(u64, u64), PathBuf> = HashMap::new();

        for (_sid, session) in sessions.iter() {
            let stream_path = session.dir.join("stream.bin");
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
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new(current_session_id, now_ms);
        omnish_context::build_context(&strategy, &formatter, &all_commands, &reader).await
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
            let mgr = SessionManager::new(base.clone());
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
            }).await.unwrap();
            // Drop the manager (simulates daemon shutdown)
        }

        // Create a new manager on the same directory and load existing sessions
        let mgr2 = SessionManager::new(base);
        let count = mgr2.load_existing().await.unwrap();
        assert_eq!(count, 1);

        let commands = mgr2.get_commands("sess1").await.unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command_line.as_deref(), Some("ls"));

        let active = mgr2.list_active().await;
        assert!(active.contains(&"sess1".to_string()));
    }
}
