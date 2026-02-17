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

    pub async fn register(
        &self,
        session_id: &str,
        parent_session_id: Option<String>,
        attrs: std::collections::HashMap<String, String>,
    ) -> Result<()> {
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

        let mut sessions = self.sessions.lock().await;
        sessions.insert(
            session_id.to_string(),
            ActiveSession {
                meta,
                stream_writer,
                commands: Vec::new(),
                dir: session_dir,
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

    pub async fn receive_command(&self, session_id: &str, record: CommandRecord) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.commands.push(record);
            CommandRecord::save_all(&session.commands, &session.dir)?;
        }
        Ok(())
    }

    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(mut session) = sessions.remove(session_id) {
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
        sessions.keys().cloned().collect()
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

        if all_commands.is_empty() {
            return Ok(String::new());
        }

        let reader = MultiSessionReader { readers: offset_to_path };
        let strategy = RecentCommands::new();
        let formatter = GroupedFormatter::new(current_session_id, now_ms);
        omnish_context::build_context(&strategy, &formatter, &all_commands, &reader).await
    }
}
