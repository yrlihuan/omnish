use anyhow::{anyhow, Result};
use omnish_llm::context::ContextBuilder;
use omnish_store::session::SessionMeta;
use omnish_store::stream::{read_entries, StreamWriter};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;

struct ActiveSession {
    meta: SessionMeta,
    stream_writer: StreamWriter,
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

    pub async fn end_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if let Some(mut session) = sessions.remove(session_id) {
            session.meta.ended_at = Some(chrono::Utc::now().to_rfc3339());
            session.meta.save(&session.dir)?;
        }
        Ok(())
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

        let stream_path = session.dir.join("stream.bin");
        let entries = read_entries(&stream_path)?;

        let mut raw_bytes = Vec::new();
        for entry in entries {
            raw_bytes.extend_from_slice(&entry.data);
        }

        let builder = ContextBuilder::new().max_chars(8000);
        let cleaned = builder.strip_escapes(&raw_bytes);
        Ok(builder.truncate(&cleaned).to_string())
    }

    pub async fn get_all_sessions_context(&self) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let mut combined_bytes = Vec::new();

        for (sid, session) in sessions.iter() {
            let header = format!("\n=== Session {} ===\n", sid);
            combined_bytes.extend_from_slice(header.as_bytes());

            let stream_path = session.dir.join("stream.bin");
            if let Ok(entries) = read_entries(&stream_path) {
                for entry in entries {
                    combined_bytes.extend_from_slice(&entry.data);
                }
            }
            combined_bytes.push(b'\n');
        }

        let builder = ContextBuilder::new().max_chars(16000);
        let cleaned = builder.strip_escapes(&combined_bytes);
        Ok(builder.truncate(&cleaned).to_string())
    }
}
