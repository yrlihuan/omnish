use omnish_protocol::message::ChatTurn;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct StoredMessage {
    role: String,
    content: String,
    ts: String,
}

pub struct ConversationManager {
    threads_dir: PathBuf,
}

impl ConversationManager {
    pub fn new(threads_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&threads_dir).ok();
        Self { threads_dir }
    }

    /// Create a new thread, return its UUID.
    pub fn create_thread(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let path = self.threads_dir.join(format!("{}.jsonl", id));
        std::fs::File::create(&path).ok();
        id
    }

    /// Get the most recent thread by file modification time, or None.
    pub fn get_latest_thread(&self) -> Option<String> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.threads_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .collect();
        entries.sort_by_key(|e| {
            std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok()))
        });
        entries.first().map(|e| {
            e.path().file_stem().unwrap().to_string_lossy().to_string()
        })
    }

    /// Append a user+assistant exchange to a thread file.
    pub fn append_exchange(&self, thread_id: &str, query: &str, response: &str) {
        use std::io::Write;
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let now = chrono::Utc::now().to_rfc3339();
            let user_msg = StoredMessage {
                role: "user".into(),
                content: query.into(),
                ts: now.clone(),
            };
            let asst_msg = StoredMessage {
                role: "assistant".into(),
                content: response.into(),
                ts: now,
            };
            writeln!(file, "{}", serde_json::to_string(&user_msg).unwrap()).ok();
            writeln!(file, "{}", serde_json::to_string(&asst_msg).unwrap()).ok();
        }
    }

    /// Get the last exchange and count of earlier messages.
    pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32) {
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return (None, 0),
        };
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        let total_messages = lines.len() as u32;
        if lines.len() < 2 {
            return (None, 0);
        }
        if let (Ok(user), Ok(asst)) = (
            serde_json::from_str::<StoredMessage>(lines[lines.len() - 2]),
            serde_json::from_str::<StoredMessage>(lines[lines.len() - 1]),
        ) {
            let earlier = total_messages.saturating_sub(2);
            (Some((user.content, asst.content)), earlier)
        } else {
            (None, 0)
        }
    }

    /// Load all messages as ChatTurn vec for LLM context.
    pub fn load_messages(&self, thread_id: &str) -> Vec<ChatTurn> {
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        content
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<StoredMessage>(l).ok())
            .map(|m| ChatTurn {
                role: m.role,
                content: m.content,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get_latest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.get_latest_thread().is_none());
        let id = mgr.create_thread();
        assert_eq!(mgr.get_latest_thread(), Some(id));
    }

    #[test]
    fn test_append_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_exchange(&id, "hello", "hi there");
        mgr.append_exchange(&id, "how are you", "doing well");
        let msgs = mgr.load_messages(&id);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "hi there");
        assert_eq!(msgs[2].content, "how are you");
        assert_eq!(msgs[3].content, "doing well");
    }

    #[test]
    fn test_get_last_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        let (ex, count) = mgr.get_last_exchange(&id);
        assert!(ex.is_none());
        assert_eq!(count, 0);

        mgr.append_exchange(&id, "q1", "a1");
        mgr.append_exchange(&id, "q2", "a2");

        let (ex, count) = mgr.get_last_exchange(&id);
        assert_eq!(ex, Some(("q2".into(), "a2".into())));
        assert_eq!(count, 2);
    }

    #[test]
    fn test_empty_thread_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        let msgs = mgr.load_messages(&id);
        assert!(msgs.is_empty());
    }
}
