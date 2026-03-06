use omnish_protocol::message::ChatTurn;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Clone)]
struct StoredMessage {
    role: String,
    content: String,
    ts: String,
}

pub struct ConversationManager {
    threads_dir: PathBuf,
    /// In-memory store: thread_id → raw messages (before interrupt resolution).
    threads: Mutex<HashMap<String, Vec<StoredMessage>>>,
}

impl ConversationManager {
    pub fn new(threads_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&threads_dir).ok();

        // Load all existing threads from disk
        let mut threads = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&threads_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map_or(true, |ext| ext != "jsonl") {
                    continue;
                }
                let thread_id = match path.file_stem() {
                    Some(s) => s.to_string_lossy().to_string(),
                    None => continue,
                };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let msgs: Vec<StoredMessage> = content
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                threads.insert(thread_id, msgs);
            }
        }
        tracing::info!("Loaded {} conversation threads from disk", threads.len());

        Self { threads_dir, threads: Mutex::new(threads) }
    }

    /// Create a new thread, return its UUID.
    pub fn create_thread(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        // Create empty file on disk
        let path = self.threads_dir.join(format!("{}.jsonl", id));
        std::fs::File::create(&path).ok();
        // Insert empty vec in memory
        self.threads.lock().unwrap().insert(id.clone(), Vec::new());
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

    /// List all conversations, sorted by modification time (newest first).
    /// Returns vector of (thread_id, last_modified, exchange_count, last_question).
    pub fn list_conversations(&self) -> Vec<(String, std::time::SystemTime, u32, String)> {
        let threads = self.threads.lock().unwrap();
        let mut conversations: Vec<_> = threads
            .iter()
            .filter_map(|(thread_id, msgs)| {
                let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
                let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
                let resolved = Self::resolve_interrupted(msgs.clone());
                let exchange_count = (resolved.len() / 2) as u32;
                let last_question = if resolved.len() >= 2 {
                    resolved[resolved.len() - 2].content.clone()
                } else {
                    String::new()
                };
                Some((thread_id.clone(), modified, exchange_count, last_question))
            })
            .collect();
        conversations.sort_by(|a, b| b.1.cmp(&a.1));
        conversations
    }

    /// Get thread_id by index (0-based, sorted by modification time).
    /// Returns None if index is out of bounds.
    pub fn get_thread_by_index(&self, index: usize) -> Option<String> {
        let conversations = self.list_conversations();
        conversations.into_iter().nth(index).map(|(thread_id, _, _, _)| thread_id)
    }

    /// Delete a thread by ID. Removes from both memory and disk.
    /// Returns true if the thread existed and was deleted.
    pub fn delete_thread(&self, thread_id: &str) -> bool {
        let removed = self.threads.lock().unwrap().remove(thread_id).is_some();
        if removed {
            let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
            std::fs::remove_file(&path).ok();
        }
        removed
    }

    /// Append a user+assistant exchange. Writes to both memory and disk (append-only).
    pub fn append_exchange(&self, thread_id: &str, query: &str, response: &str) {
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

        // Update memory
        self.threads
            .lock()
            .unwrap()
            .entry(thread_id.to_string())
            .or_default()
            .extend([user_msg.clone(), asst_msg.clone()]);

        // Append to disk
        use std::io::Write;
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            writeln!(file, "{}", serde_json::to_string(&user_msg).unwrap()).ok();
            writeln!(file, "{}", serde_json::to_string(&asst_msg).unwrap()).ok();
        }
    }

    /// Get the last exchange and count of earlier messages (after resolving interrupts).
    pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32) {
        let threads = self.threads.lock().unwrap();
        let msgs = match threads.get(thread_id) {
            Some(m) => m.clone(),
            None => return (None, 0),
        };
        drop(threads);
        let resolved = Self::resolve_interrupted(msgs);
        let total = resolved.len() as u32;
        if resolved.len() < 2 {
            return (None, 0);
        }
        let asst = &resolved[resolved.len() - 1];
        let user = &resolved[resolved.len() - 2];
        let earlier = total.saturating_sub(2);
        (Some((user.content.clone(), asst.content.clone())), earlier)
    }

    /// Load all messages as ChatTurn vec for LLM context.
    /// Resolves interrupt conflicts.
    pub fn load_messages(&self, thread_id: &str) -> Vec<ChatTurn> {
        let threads = self.threads.lock().unwrap();
        let msgs = match threads.get(thread_id) {
            Some(m) => m.clone(),
            None => return vec![],
        };
        drop(threads);
        Self::resolve_interrupted(msgs)
            .into_iter()
            .map(|m| ChatTurn { role: m.role, content: m.content })
            .collect()
    }

    const INTERRUPTED_MARKER: &str = "<event>user interrupted</event>";

    /// Resolve interrupt conflicts in a list of stored messages.
    ///
    /// An interrupted exchange is a (user, assistant) pair where the assistant
    /// content is the interrupted marker. For each such pair, any other exchange
    /// with the same user query is dropped — the interrupt always wins.
    fn resolve_interrupted(msgs: Vec<StoredMessage>) -> Vec<StoredMessage> {
        // Parse into exchanges (user + assistant pairs)
        let mut exchanges: Vec<(StoredMessage, StoredMessage)> = Vec::new();
        let mut iter = msgs.into_iter();
        while let Some(first) = iter.next() {
            if let Some(second) = iter.next() {
                exchanges.push((first, second));
            }
        }

        // Collect all queries that have an interrupted exchange
        let interrupted_queries: std::collections::HashSet<String> = exchanges
            .iter()
            .filter(|(_, a)| a.content == Self::INTERRUPTED_MARKER)
            .map(|(u, _)| u.content.clone())
            .collect();

        // Keep exchanges that are either:
        // - not in the interrupted set, or
        // - the interrupted version itself
        exchanges
            .into_iter()
            .filter(|(u, a)| {
                !interrupted_queries.contains(&u.content)
                    || a.content == Self::INTERRUPTED_MARKER
            })
            .flat_map(|(u, a)| [u, a])
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

    #[test]
    fn test_interrupt_before_response_resolved() {
        // Case 2: interrupt arrives first, then LLM response
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_exchange(&id, "q1", "a1");
        // Interrupt arrives first
        mgr.append_exchange(&id, "give me riddles", "<event>user interrupted</event>");
        // LLM response arrives later
        mgr.append_exchange(&id, "give me riddles", "here are some riddles...");

        let msgs = mgr.load_messages(&id);
        // Should have: q1/a1 + interrupted exchange only (LLM response dropped)
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[2].content, "give me riddles");
        assert_eq!(msgs[3].content, "<event>user interrupted</event>");

        let (ex, count) = mgr.get_last_exchange(&id);
        assert_eq!(ex, Some(("give me riddles".into(), "<event>user interrupted</event>".into())));
        assert_eq!(count, 2);
    }

    #[test]
    fn test_interrupt_after_response_resolved() {
        // Case 3: LLM response arrives first, then interrupt
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_exchange(&id, "q1", "a1");
        // LLM response arrives first
        mgr.append_exchange(&id, "give me riddles", "here are some riddles...");
        // Interrupt arrives later
        mgr.append_exchange(&id, "give me riddles", "<event>user interrupted</event>");

        let msgs = mgr.load_messages(&id);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[2].content, "give me riddles");
        assert_eq!(msgs[3].content, "<event>user interrupted</event>");

        let (ex, count) = mgr.get_last_exchange(&id);
        assert_eq!(ex, Some(("give me riddles".into(), "<event>user interrupted</event>".into())));
        assert_eq!(count, 2);
    }

    #[test]
    fn test_no_interrupt_unchanged() {
        // Normal conversation without interrupts should be unchanged
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();
        mgr.append_exchange(&id, "q1", "a1");
        mgr.append_exchange(&id, "q2", "a2");
        let msgs = mgr.load_messages(&id);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1].content, "a1");
        assert_eq!(msgs[3].content, "a2");
    }

    #[test]
    fn test_delete_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id1 = mgr.create_thread();
        let id2 = mgr.create_thread();
        mgr.append_exchange(&id1, "q1", "a1");
        mgr.append_exchange(&id2, "q2", "a2");

        // Delete first thread
        assert!(mgr.delete_thread(&id1));
        assert!(mgr.load_messages(&id1).is_empty());
        assert!(!dir.path().join(format!("{}.jsonl", id1)).exists());

        // Second thread still intact
        assert_eq!(mgr.load_messages(&id2).len(), 2);

        // Deleting again returns false
        assert!(!mgr.delete_thread(&id1));

        // Deleted thread not in list
        let convs = mgr.list_conversations();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].0, id2);
    }

    #[test]
    fn test_delete_thread_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mgr1 = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr1.create_thread();
        mgr1.append_exchange(&id, "hello", "world");
        mgr1.delete_thread(&id);
        drop(mgr1);

        // New manager should not find the deleted thread
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr2.load_messages(&id).is_empty());
        assert!(mgr2.list_conversations().is_empty());
    }

    #[test]
    fn test_load_from_disk_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        // Create a thread and add data with first manager instance
        let mgr1 = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr1.create_thread();
        mgr1.append_exchange(&id, "hello", "world");
        drop(mgr1);

        // Create new manager — should load from disk
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        let msgs = mgr2.load_messages(&id);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "world");
    }
}
