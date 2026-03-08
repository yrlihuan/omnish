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
