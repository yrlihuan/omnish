use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ThreadUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ThreadMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Number of conversation rounds when summary was last generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_rounds: Option<u32>,
    /// Backend name for per-thread model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Last LLM call usage (for display in /thread stats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_last: Option<ThreadUsage>,
    /// Cumulative usage for the current model (resets on model switch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_total: Option<ThreadUsage>,
    /// Name of the model that produced usage_last/usage_total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    /// Last system-reminder content (for change detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_reminder: Option<String>,
    /// Per-thread sandbox override. When Some(true), daemon forces
    /// ChatToolCall.sandboxed=false for this thread, bypassing permit_rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_disabled: Option<bool>,
}

pub struct ConversationManager {
    threads_dir: PathBuf,
    /// In-memory store: thread_id → raw JSON messages.
    threads: Mutex<HashMap<String, Vec<serde_json::Value>>>,
}

/// Extract tool_use IDs from an assistant message's content array.
fn extract_tool_use_ids(msg: &serde_json::Value) -> Vec<String> {
    if msg["role"].as_str() != Some("assistant") {
        return Vec::new();
    }
    let content = match msg["content"].as_array() {
        Some(arr) => arr,
        None => return Vec::new(),
    };
    content
        .iter()
        .filter(|b| b["type"].as_str() == Some("tool_use"))
        .filter_map(|b| b["id"].as_str().map(|s| s.to_string()))
        .collect()
}

/// Extract tool_result tool_use_ids from a user message's content array.
fn extract_tool_result_ids(msg: &serde_json::Value) -> HashSet<String> {
    if msg["role"].as_str() != Some("user") {
        return HashSet::new();
    }
    let content = match msg["content"].as_array() {
        Some(arr) => arr,
        None => return HashSet::new(),
    };
    content
        .iter()
        .filter(|b| b["type"].as_str() == Some("tool_result"))
        .filter_map(|b| b["tool_use_id"].as_str().map(|s| s.to_string()))
        .collect()
}

/// Scan for assistant messages with tool_use blocks that lack corresponding
/// tool_result blocks in the following message. Inject synthetic error
/// tool_result messages so the API never sees orphaned tool_use blocks.
/// Returns true if any changes were made.
fn sanitize_orphaned_tool_use(msgs: &mut Vec<serde_json::Value>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < msgs.len() {
        let tool_use_ids = extract_tool_use_ids(&msgs[i]);
        if tool_use_ids.is_empty() {
            i += 1;
            continue;
        }

        // Check if the next message provides matching tool_results
        let next_result_ids: HashSet<String> = if i + 1 < msgs.len() {
            extract_tool_result_ids(&msgs[i + 1])
        } else {
            HashSet::new()
        };

        let missing: Vec<String> = tool_use_ids
            .into_iter()
            .filter(|id| !next_result_ids.contains(id))
            .collect();

        if missing.is_empty() {
            i += 2; // skip past assistant(tool_use) + user(tool_result)
            continue;
        }

        changed = true;

        let has_partial_results =
            i + 1 < msgs.len() && !next_result_ids.is_empty();

        if has_partial_results {
            // Extend existing tool_result message with the missing IDs
            if let Some(arr) = msgs[i + 1]["content"].as_array_mut() {
                for id in &missing {
                    arr.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": "tool execution was interrupted — results unavailable",
                        "is_error": true,
                    }));
                }
            }
            i += 2;
        } else {
            // Insert a new user message with synthetic tool_results
            let content: Vec<serde_json::Value> = missing
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": "tool execution was interrupted — results unavailable",
                        "is_error": true,
                    })
                })
                .collect();
            msgs.insert(i + 1, serde_json::json!({
                "role": "user",
                "content": content,
            }));
            i += 2;
        }
    }
    changed
}

impl ConversationManager {
    pub fn new(threads_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&threads_dir).ok();

        // Load all existing threads from disk
        let mut threads = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&threads_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "jsonl") {
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
                let mut msgs: Vec<serde_json::Value> = content
                    .lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                if sanitize_orphaned_tool_use(&mut msgs) {
                    tracing::warn!(
                        "Sanitized orphaned tool_use blocks in thread {} at startup",
                        thread_id
                    );
                    Self::rewrite_thread_file(&threads_dir, &thread_id, &msgs);
                }
                threads.insert(thread_id, msgs);
            }
        }
        tracing::info!("Loaded {} conversation threads from disk", threads.len());

        Self { threads_dir, threads: Mutex::new(threads) }
    }

    /// Create a new thread with optional metadata, return its UUID.
    pub fn create_thread(&self, meta: ThreadMeta) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        // Create empty file on disk
        let path = self.threads_dir.join(format!("{}.jsonl", id));
        std::fs::File::create(&path).ok();
        // Save metadata
        self.save_meta(&id, &meta);
        // Insert empty vec in memory
        self.threads.lock().unwrap().insert(id.clone(), Vec::new());
        id
    }

    /// Save thread metadata to a sidecar `.meta.json` file.
    pub fn save_meta(&self, thread_id: &str, meta: &ThreadMeta) {
        let path = self.threads_dir.join(format!("{}.meta.json", thread_id));
        if let Ok(json) = serde_json::to_string_pretty(meta) {
            std::fs::write(&path, json).ok();
        }
    }

    /// Set per-thread sandbox override and persist. `disabled=true` sets the
    /// override; `disabled=false` clears it (back to default "sandbox on").
    pub fn set_sandbox_disabled(&self, thread_id: &str, disabled: bool) {
        let mut meta = self.load_meta(thread_id);
        meta.sandbox_disabled = if disabled { Some(true) } else { None };
        self.save_meta(thread_id, &meta);
    }

    /// Load thread metadata from the sidecar `.meta.json` file.
    pub fn load_meta(&self, thread_id: &str) -> ThreadMeta {
        let path = self.threads_dir.join(format!("{}.meta.json", thread_id));
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Get the most recent thread by file modification time, or None.
    pub fn get_latest_thread(&self) -> Option<String> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.threads_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        entries.sort_by_key(|e| {
            std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok()))
        });
        entries.first().map(|e| {
            e.path().file_stem().unwrap().to_string_lossy().to_string()
        })
    }

    /// Check whether a thread exists (has been created and not deleted).
    pub fn thread_exists(&self, thread_id: &str) -> bool {
        self.threads.lock().unwrap().contains_key(thread_id)
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
                    .map(Self::extract_text)
                    .unwrap_or_default();
                Some((thread_id.clone(), modified, exchange_count, last_question))
            })
            .collect();
        conversations.sort_by_key(|c| std::cmp::Reverse(c.1));
        conversations
    }

    /// List all thread IDs.
    pub fn list_thread_ids(&self) -> Vec<String> {
        self.threads.lock().unwrap().keys().cloned().collect()
    }

    /// Count conversation rounds (user inputs) in a thread.
    pub fn count_rounds(&self, thread_id: &str) -> u32 {
        let threads = self.threads.lock().unwrap();
        threads
            .get(thread_id)
            .map(|msgs| msgs.iter().filter(|m| Self::is_user_input(m)).count() as u32)
            .unwrap_or(0)
    }

    /// Get thread_id by index (0-based, sorted by modification time).
    /// Returns None if index is out of bounds.
    pub fn get_thread_by_index(&self, index: usize) -> Option<String> {
        let conversations = self.list_conversations();
        conversations.into_iter().nth(index).map(|(thread_id, _, _, _)| thread_id)
    }

    /// Delete a thread by ID. Removes from both memory and disk (including metadata).
    /// Returns true if the thread existed and was deleted.
    pub fn delete_thread(&self, thread_id: &str) -> bool {
        let removed = self.threads.lock().unwrap().remove(thread_id).is_some();
        if removed {
            let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
            std::fs::remove_file(&path).ok();
            let meta_path = self.threads_dir.join(format!("{}.meta.json", thread_id));
            std::fs::remove_file(&meta_path).ok();
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

    /// Rewrite a thread's JSONL file from the given messages.
    fn rewrite_thread_file(
        threads_dir: &std::path::Path,
        thread_id: &str,
        msgs: &[serde_json::Value],
    ) {
        use std::io::Write;
        let path = threads_dir.join(format!("{}.jsonl", thread_id));
        if let Ok(mut file) = std::fs::File::create(&path) {
            for msg in msgs {
                writeln!(file, "{}", serde_json::to_string(msg).unwrap()).ok();
            }
        }
    }

    /// Load all messages as raw JSON for LLM context.
    /// Note: orphaned tool_use blocks are only sanitized at startup (in `new()`),
    /// not here — during runtime an orphaned tail tool_use means tools are actively
    /// executing, and sanitizing it would inject a phantom "interrupted" result.
    pub fn load_raw_messages(&self, thread_id: &str) -> Vec<serde_json::Value> {
        let threads = self.threads.lock().unwrap();
        let msgs = match threads.get(thread_id) {
            Some(m) => m,
            None => return Vec::new(),
        };
        msgs.clone()
    }

    /// Get all user-assistant exchanges in a thread, ordered chronologically.
    /// Returns Vec of (user_text, assistant_text) pairs.
    pub fn get_all_exchanges(&self, thread_id: &str) -> Vec<(String, String)> {
        let threads = self.threads.lock().unwrap();
        let msgs = match threads.get(thread_id) {
            Some(m) => m.clone(),
            None => return Vec::new(),
        };
        drop(threads);

        let mut exchanges = Vec::new();
        let mut i = 0;
        while i < msgs.len() {
            if Self::is_user_input(&msgs[i]) {
                let user_text = Self::extract_text(&msgs[i]);
                // Collect assistant text after this user message until next user input
                let mut assistant_parts = Vec::new();
                let mut j = i + 1;
                while j < msgs.len() && !Self::is_user_input(&msgs[j]) {
                    if msgs[j]["role"].as_str() == Some("assistant") {
                        let text = Self::extract_text(&msgs[j]);
                        if !text.is_empty() {
                            assistant_parts.push(text);
                        }
                    }
                    j += 1;
                }
                exchanges.push((user_text, assistant_parts.join("\n")));
                i = j;
            } else {
                i += 1;
            }
        }
        exchanges
    }

    /// Check if a message is a user input message (content is a string, not tool_result array).
    fn is_user_input(msg: &serde_json::Value) -> bool {
        msg["role"].as_str() == Some("user") && msg["content"].is_string()
    }

    /// Extract display text from a message, stripping <system-reminder> blocks.
    fn extract_text(msg: &serde_json::Value) -> String {
        match &msg["content"] {
            serde_json::Value::String(s) => s.clone(),
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

    /// Collect conversations from threads modified since `since`, formatted as markdown.
    /// Each thread becomes a `## Thread: {title}` section with User/Assistant exchanges.
    /// Filters out tool_use/tool_result blocks, keeping only text content.
    pub fn collect_recent_conversations_md(&self, since: std::time::SystemTime) -> String {
        let conversations = self.list_conversations();
        let mut result = String::new();

        for (thread_id, mtime, _count, _last_q) in &conversations {
            if *mtime < since {
                continue;
            }

            let meta = self.load_meta(thread_id);
            let title = meta.summary.unwrap_or_else(|| "untitled".to_string());

            let messages = self.load_raw_messages(thread_id);
            if messages.is_empty() {
                continue;
            }

            let mut thread_md = format!("## Thread: {}\n\n", title);
            for msg in &messages {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let text = Self::extract_text(msg);
                if text.is_empty() {
                    continue;
                }
                let label = match role {
                    "user" => "User",
                    "assistant" => "Assistant",
                    _ => continue,
                };
                thread_md.push_str(&format!("{}: {}\n\n", label, text));
            }

            result.push_str(&thread_md);
        }

        result
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

        let id = mgr.create_thread(ThreadMeta::default());
        let latest = mgr.get_latest_thread().unwrap();
        assert_eq!(latest, id);
    }

    #[test]
    fn test_append_and_load_raw() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

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
    fn test_get_all_exchanges() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        // Empty thread
        let exchanges = mgr.get_all_exchanges(&id);
        assert!(exchanges.is_empty());

        // After first exchange
        mgr.append_messages(&id, &[user_msg("first question"), assistant_msg("first answer")]);
        let exchanges = mgr.get_all_exchanges(&id);
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].0, "first question");
        assert_eq!(exchanges[0].1, "first answer");

        // After second exchange
        mgr.append_messages(&id, &[user_msg("second question"), assistant_msg("second answer")]);
        let exchanges = mgr.get_all_exchanges(&id);
        assert_eq!(exchanges.len(), 2);
        assert_eq!(exchanges[0].0, "first question");
        assert_eq!(exchanges[0].1, "first answer");
        assert_eq!(exchanges[1].0, "second question");
        assert_eq!(exchanges[1].1, "second answer");
    }

    #[test]
    fn test_empty_thread_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        let msgs = mgr.load_raw_messages(&id);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_tool_use_messages_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

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
    fn test_get_all_exchanges_with_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        let messages = vec![
            user_msg("what did ls output?"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("Here's what I found"),
        ];
        mgr.append_messages(&id, &messages);

        let exchanges = mgr.get_all_exchanges(&id);
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].0, "what did ls output?");
        // Both assistant messages' text concatenated
        assert_eq!(exchanges[0].1, "Let me check...\nHere's what I found");
    }

    #[test]
    fn test_plain_query_stored_without_system_reminder() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        // system-reminder is NOT stored — server strips it before persisting
        mgr.append_messages(&id, &[user_msg("what happened?"), assistant_msg("Everything is fine")]);

        let exchanges = mgr.get_all_exchanges(&id);
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].0, "what happened?");
        assert_eq!(exchanges[0].1, "Everything is fine");
    }

    #[test]
    fn test_interrupt_stored_as_raw_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

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

        let id1 = mgr.create_thread(ThreadMeta::default());
        let id2 = mgr.create_thread(ThreadMeta::default());
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
            thread_id = mgr.create_thread(ThreadMeta::default());
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

    #[test]
    fn test_meta_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        let meta = ThreadMeta {
            host: Some("myhost".to_string()),
            cwd: Some("/home/user".to_string()),
            ..Default::default()
        };
        let id = mgr.create_thread(meta);

        let loaded = mgr.load_meta(&id);
        assert_eq!(loaded.host.as_deref(), Some("myhost"));
        assert_eq!(loaded.cwd.as_deref(), Some("/home/user"));

        // Meta file exists on disk
        let meta_path = dir.path().join(format!("{}.meta.json", id));
        assert!(meta_path.exists());
    }

    #[test]
    fn test_meta_default_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        let loaded = mgr.load_meta(&id);
        assert!(loaded.host.is_none());
        assert!(loaded.cwd.is_none());
    }

    #[test]
    fn test_delete_thread_removes_meta() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        let meta = ThreadMeta {
            host: Some("host".to_string()),
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        };
        let id = mgr.create_thread(meta);
        let meta_path = dir.path().join(format!("{}.meta.json", id));
        assert!(meta_path.exists());

        mgr.delete_thread(&id);
        assert!(!meta_path.exists());
    }

    #[test]
    fn test_set_sandbox_disabled_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        let tid = mgr.create_thread(ThreadMeta::default());
        assert_eq!(mgr.load_meta(&tid).sandbox_disabled, None);

        mgr.set_sandbox_disabled(&tid, true);
        assert_eq!(mgr.load_meta(&tid).sandbox_disabled, Some(true));

        mgr.set_sandbox_disabled(&tid, false);
        assert_eq!(mgr.load_meta(&tid).sandbox_disabled, None);
    }

    #[test]
    fn test_sanitize_orphaned_tool_use_no_result() {
        // assistant(tool_use) with no following tool_result → inject synthetic result
        let mut msgs = vec![
            user_msg("do something"),
            assistant_with_tool_use(),
        ];
        assert!(sanitize_orphaned_tool_use(&mut msgs));
        assert_eq!(msgs.len(), 3);
        // Injected message should be a user message with tool_result
        assert_eq!(msgs[2]["role"], "user");
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_1");
        assert_eq!(content[0]["is_error"], true);
    }

    #[test]
    fn test_sanitize_orphaned_tool_use_followed_by_user_query() {
        // assistant(tool_use) followed by user(text) instead of tool_result
        let mut msgs = vec![
            user_msg("do something"),
            assistant_with_tool_use(),
            user_msg("what happened?"),
        ];
        assert!(sanitize_orphaned_tool_use(&mut msgs));
        assert_eq!(msgs.len(), 4);
        // Synthetic tool_result injected between assistant and next user message
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"].as_array().unwrap()[0]["type"], "tool_result");
        assert_eq!(msgs[3]["content"], "what happened?");
    }

    #[test]
    fn test_sanitize_valid_tool_use_unchanged() {
        // assistant(tool_use) + user(tool_result) → no changes
        let mut msgs = vec![
            user_msg("do something"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("done"),
        ];
        assert!(!sanitize_orphaned_tool_use(&mut msgs));
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn test_sanitize_partial_tool_results() {
        // assistant with two tool_use blocks, but only one tool_result
        let mut msgs = vec![
            user_msg("do something"),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "tool_a", "input": {}},
                    {"type": "tool_use", "id": "toolu_2", "name": "tool_b", "input": {}},
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok", "is_error": false},
                ]
            }),
            assistant_msg("partial done"),
        ];
        assert!(sanitize_orphaned_tool_use(&mut msgs));
        // Should still be 4 messages, but the tool_result message should now have 2 entries
        assert_eq!(msgs.len(), 4);
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["tool_use_id"], "toolu_2");
        assert_eq!(content[1]["is_error"], true);
    }

    #[test]
    fn test_thread_meta_sandbox_disabled_roundtrip() {
        // Default ThreadMeta: sandbox_disabled is None and omitted from JSON.
        let meta = ThreadMeta::default();
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("sandbox_disabled"),
            "absent flag must not appear in JSON, got: {}", json);

        // sandbox_disabled=Some(true) roundtrips.
        let meta_off = ThreadMeta { sandbox_disabled: Some(true), ..ThreadMeta::default() };
        let json = serde_json::to_string(&meta_off).unwrap();
        assert!(json.contains("\"sandbox_disabled\":true"),
            "flag must appear when set, got: {}", json);
        let parsed: ThreadMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sandbox_disabled, Some(true));

        // JSON without the field loads as None (pre-feature threads).
        let legacy = "{}";
        let parsed: ThreadMeta = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.sandbox_disabled, None);
    }

    #[test]
    fn test_sanitize_persisted_on_load() {
        // Write an orphaned tool_use to disk, then load — should be sanitized
        let dir = tempfile::tempdir().unwrap();
        let thread_id;
        {
            let mgr = ConversationManager::new(dir.path().to_path_buf());
            thread_id = mgr.create_thread(ThreadMeta::default());
            // Simulate persist_unsaved saving tool_use without tool_result
            mgr.append_messages(&thread_id, &[
                user_msg("run tool"),
                assistant_with_tool_use(),
            ]);
        }
        // Reload from disk — startup sanitization should fix it
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        let msgs = mgr2.load_raw_messages(&thread_id);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"].as_array().unwrap()[0]["type"], "tool_result");

        // Verify the fix was persisted to disk
        let path = dir.path().join(format!("{}.jsonl", thread_id));
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
    }
}
