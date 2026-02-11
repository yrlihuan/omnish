use anyhow::Result;
use omnish_store::session::SessionMeta;
use omnish_store::stream::StreamWriter;
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
        shell: &str,
        pid: u32,
        tty: &str,
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
            shell: shell.to_string(),
            pid,
            tty: tty.to_string(),
            started_at: now,
            ended_at: None,
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
}
