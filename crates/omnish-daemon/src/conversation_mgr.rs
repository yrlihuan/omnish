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

    /// List all conversations, sorted by modification time (newest first).
    /// Returns vector of (thread_id, last_modified, exchange_count, last_question).
    pub fn list_conversations(&self) -> Vec<(String, std::time::SystemTime, u32, String)> {
        let entries: Vec<_> = std::fs::read_dir(&self.threads_dir)
            .ok()
            .map(|e| e.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();

        let mut conversations: Vec<_> = entries
            .into_iter()
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .filter_map(|e| {
                let path = e.path();
                let thread_id = path.file_stem()?.to_string_lossy().to_string();
                let metadata = e.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                let content = std::fs::read_to_string(&path).ok()?;
                let msgs: Vec<StoredMessage> = content
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                let resolved = Self::resolve_interrupted(msgs);
                let exchange_count = (resolved.len() / 2) as u32;
                // Get last user question (second-to-last message, or None if no messages)
                let last_question = if resolved.len() >= 2 {
                    resolved[resolved.len() - 2].content.clone()
                } else {
                    String::new()
                };
                Some((thread_id, modified, exchange_count, last_question))
            })
            .collect();

        // Sort by modification time, newest first
        conversations.sort_by(|a, b| b.1.cmp(&a.1));
        conversations
    }

    /// Get thread_id by index (0-based, sorted by modification time).
    /// Returns None if index is out of bounds.
    pub fn get_thread_by_index(&self, index: usize) -> Option<String> {
        let conversations = self.list_conversations();
        conversations.into_iter().nth(index).map(|(thread_id, _, _, _)| thread_id)
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

    /// Get the last exchange and count of earlier messages (after resolving interrupts).
    pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32) {
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return (None, 0),
        };
        let msgs: Vec<StoredMessage> = content
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
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
    /// Resolves interrupt conflicts: collects all interrupted (query, ts) pairs,
    /// then drops any non-interrupted exchange whose query+ts is superseded by an interrupt.
    pub fn load_messages(&self, thread_id: &str) -> Vec<ChatTurn> {
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let msgs: Vec<StoredMessage> = content
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
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
}
