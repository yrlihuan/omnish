use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct ConversationManager {
    threads_dir: PathBuf,
    /// In-memory store: thread_id → raw JSON messages.
    threads: Mutex<HashMap<String, Vec<serde_json::Value>>>,
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
                let msgs: Vec<serde_json::Value> = content
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
                let exchange_count = msgs.iter()
                    .filter(|m| Self::is_user_input(m))
                    .count() as u32;
                let last_question = msgs.iter()
                    .rev()
                    .find(|m| Self::is_user_input(m))
                    .map(|m| Self::extract_text(m))
                    .unwrap_or_default();
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

    /// Append raw JSON messages. Writes to both memory and disk (append-only).
    pub fn append_messages(&self, thread_id: &str, messages: &[serde_json::Value]) {
        self.threads.lock().unwrap()
            .entry(thread_id.to_string())
            .or_default()
            .extend(messages.iter().cloned());

        use std::io::Write;
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path)
        {
            for msg in messages {
                writeln!(file, "{}", serde_json::to_string(msg).unwrap()).ok();
            }
        }
    }

    /// Load all messages as raw JSON for LLM context.
    pub fn load_raw_messages(&self, thread_id: &str) -> Vec<serde_json::Value> {
        let threads = self.threads.lock().unwrap();
        threads.get(thread_id).cloned().unwrap_or_default()
    }

    /// Get the last exchange and count of earlier user input messages.
    pub fn get_last_exchange(&self, thread_id: &str) -> (Option<(String, String)>, u32) {
        let threads = self.threads.lock().unwrap();
        let msgs = match threads.get(thread_id) {
            Some(m) => m.clone(),
            None => return (None, 0),
        };
        drop(threads);

        // Count user input messages (where content is String, not Array of tool_result)
        let user_input_count = msgs.iter().filter(|m| Self::is_user_input(m)).count() as u32;
        if user_input_count == 0 {
            return (None, 0);
        }

        // Find last user input message
        let last_user_idx = msgs.iter().rposition(|m| Self::is_user_input(m));
        let last_user_idx = match last_user_idx {
            Some(idx) => idx,
            None => return (None, 0),
        };

        let user_text = Self::extract_text(&msgs[last_user_idx]);

        // Collect assistant text after the last user input
        let assistant_text: String = msgs[last_user_idx + 1..]
            .iter()
            .filter(|m| m["role"].as_str() == Some("assistant"))
            .map(|m| Self::extract_text(m))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        let earlier = user_input_count.saturating_sub(1);
        if assistant_text.is_empty() {
            // User message with no assistant response yet
            (Some((user_text, String::new())), earlier)
        } else {
            (Some((user_text, assistant_text)), earlier)
        }
    }

    /// Check if a message is a user input message (content is a string, not tool_result array).
    fn is_user_input(msg: &serde_json::Value) -> bool {
        msg["role"].as_str() == Some("user") && msg["content"].is_string()
    }

    /// Extract display text from a message, stripping <system-reminder> blocks.
    fn extract_text(msg: &serde_json::Value) -> String {
        match &msg["content"] {
            serde_json::Value::String(s) => {
                if let Some(pos) = s.find("\n\n<system-reminder>") {
                    s[..pos].to_string()
                } else {
                    s.clone()
                }
            }
            serde_json::Value::Array(arr) => {
                arr.iter()
                    .filter_map(|b| {
                        if b["type"].as_str() == Some("text") {
                            b["text"].as_str().map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => String::new(),
        }
    }

    /// Public accessor for extract_text (used by server.rs display handlers).
    pub fn extract_text_public(msg: &serde_json::Value) -> String {
        Self::extract_text(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    fn assistant_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": text})
    }

    fn assistant_with_tool_use() -> serde_json::Value {
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Let me check..."},
                {"type": "tool_use", "id": "toolu_1", "name": "command_query", "input": {"action": "get_output", "seq": 1}}
            ]
        })
    }

    fn tool_result_msg() -> serde_json::Value {
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "output data", "is_error": false}
            ]
        })
    }

    #[test]
    fn test_create_and_get_latest() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.get_latest_thread().is_none());

        let id = mgr.create_thread();
        let latest = mgr.get_latest_thread().unwrap();
        assert_eq!(latest, id);
    }

    #[test]
    fn test_append_and_load_raw() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        mgr.append_messages(&id, &[user_msg("hello"), assistant_msg("hi there")]);
        mgr.append_messages(&id, &[user_msg("how are you?"), assistant_msg("I'm fine")]);

        let msgs = mgr.load_raw_messages(&id);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hello");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "hi there");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"], "how are you?");
        assert_eq!(msgs[3]["role"], "assistant");
        assert_eq!(msgs[3]["content"], "I'm fine");
    }

    #[test]
    fn test_get_last_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        // Empty thread
        let (exchange, count) = mgr.get_last_exchange(&id);
        assert!(exchange.is_none());
        assert_eq!(count, 0);

        // After first exchange
        mgr.append_messages(&id, &[user_msg("first question"), assistant_msg("first answer")]);
        let (exchange, count) = mgr.get_last_exchange(&id);
        let (q, a) = exchange.unwrap();
        assert_eq!(q, "first question");
        assert_eq!(a, "first answer");
        assert_eq!(count, 0); // no earlier exchanges

        // After second exchange
        mgr.append_messages(&id, &[user_msg("second question"), assistant_msg("second answer")]);
        let (exchange, count) = mgr.get_last_exchange(&id);
        let (q, a) = exchange.unwrap();
        assert_eq!(q, "second question");
        assert_eq!(a, "second answer");
        assert_eq!(count, 1); // 1 earlier exchange
    }

    #[test]
    fn test_empty_thread_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        let msgs = mgr.load_raw_messages(&id);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_tool_use_messages_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        let messages = vec![
            user_msg("what did ls output?"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("Here's what I found"),
        ];
        mgr.append_messages(&id, &messages);

        let stored = mgr.load_raw_messages(&id);
        assert_eq!(stored.len(), 4);

        // tool_result is NOT a user input (content is array, not string)
        assert!(!ConversationManager::is_user_input(&stored[2]));
        // actual user input IS user input
        assert!(ConversationManager::is_user_input(&stored[0]));
    }

    #[test]
    fn test_get_last_exchange_with_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        let messages = vec![
            user_msg("what did ls output?"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("Here's what I found"),
        ];
        mgr.append_messages(&id, &messages);

        let (exchange, count) = mgr.get_last_exchange(&id);
        let (q, a) = exchange.unwrap();
        assert_eq!(q, "what did ls output?");
        // Both assistant messages' text concatenated
        assert_eq!(a, "Let me check...\nHere's what I found");
        // Only 1 user input, so 0 earlier
        assert_eq!(count, 0);
    }

    #[test]
    fn test_system_reminder_stripped_from_display() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        let user = serde_json::json!({
            "role": "user",
            "content": "what happened?\n\n<system-reminder>Recent commands:\n[seq=1] ls\n</system-reminder>"
        });
        mgr.append_messages(&id, &[user, assistant_msg("Everything is fine")]);

        let (exchange, _) = mgr.get_last_exchange(&id);
        let (q, a) = exchange.unwrap();
        assert_eq!(q, "what happened?");
        assert_eq!(a, "Everything is fine");
    }

    #[test]
    fn test_interrupt_stored_as_raw_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread();

        // Simulate interrupt: user message + partial assistant response
        let messages = vec![
            user_msg("tell me a story"),
            assistant_msg("[interrupted] Once upon a time..."),
        ];
        mgr.append_messages(&id, &messages);

        let stored = mgr.load_raw_messages(&id);
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0]["content"], "tell me a story");
        assert!(stored[1]["content"].as_str().unwrap().starts_with("[interrupted]"));
    }

    #[test]
    fn test_delete_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        let id1 = mgr.create_thread();
        let id2 = mgr.create_thread();
        mgr.append_messages(&id1, &[user_msg("msg1")]);
        mgr.append_messages(&id2, &[user_msg("msg2")]);

        // Delete first thread
        assert!(mgr.delete_thread(&id1));
        // Memory: load returns empty
        assert!(mgr.load_raw_messages(&id1).is_empty());
        // Disk: file is removed
        let path1 = dir.path().join(format!("{}.jsonl", id1));
        assert!(!path1.exists());
        // Second thread still exists
        assert_eq!(mgr.load_raw_messages(&id2).len(), 1);

        // Deleting non-existent thread returns false
        assert!(!mgr.delete_thread(&id1));
    }

    #[test]
    fn test_load_from_disk_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let thread_id;

        {
            let mgr = ConversationManager::new(dir.path().to_path_buf());
            thread_id = mgr.create_thread();
            mgr.append_messages(&thread_id, &[
                user_msg("persistent question"),
                assistant_msg("persistent answer"),
            ]);
        }
        // Original manager dropped; create a new one from the same directory
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        let msgs = mgr2.load_raw_messages(&thread_id);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["content"], "persistent question");
        assert_eq!(msgs[1]["content"], "persistent answer");
    }
}
