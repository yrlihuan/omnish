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
    /// Single English word derived from `summary`, used as the tmux window
    /// label when resuming the thread. Regenerated when `summary` changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_word: Option<String>,
    /// User-specified thread name, takes precedence over `title_word` for
    /// tmux labelling. Never touched by the summary loop; only cleared
    /// explicitly via `/thread rename` with no argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_override: Option<String>,
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
    /// Project instructions (CWD CLAUDE.md) sent with the last chat turn.
    /// Together with `system_reminder` this lets a full `/compact` replay the
    /// exact system prompt of that turn and hit the provider's prefix cache
    /// (issue #687).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_instructions: Option<String>,
    /// Per-thread sandbox override. When Some(true), daemon forces
    /// ChatToolCall.sandboxed=false for this thread, bypassing permit_rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_disabled: Option<bool>,
    /// One-shot `<event>...</event>` marker(s) queued by an out-of-band state
    /// change (e.g. `/thread sandbox on|off`, host switch on resume).
    /// Prepended to the next user message sent to the LLM, then cleared, so the
    /// model learns the change happened. Several changes before the next
    /// message are newline-joined (see `queue_event`) rather than clobbering
    /// each other. See issues #659 and #679.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_event: Option<String>,
    /// Soft-delete tombstone (epoch ms). `/thread del` sets this instead of
    /// removing the JSONL, so a deleted conversation can still be listed and
    /// restored. Tombstoned threads are skipped at load, which keeps them out
    /// of memory and therefore out of every listing/resume path (issue #684).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<u64>,
}

impl ThreadMeta {
    /// Effective tmux label: user override takes precedence over auto-generated
    /// title_word. Returns `None` when neither is set.
    pub fn effective_title_label(&self) -> Option<String> {
        self.title_override.clone().or_else(|| self.title_word.clone())
    }

    /// True when the thread has been soft-deleted (issue #684).
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Append a one-shot `<event>` marker for the next user message, preserving
    /// any marker already pending (newline-separated) so independent state
    /// changes (e.g. a host switch followed by a sandbox toggle) all reach the
    /// LLM. A pre-existing line equal to `event` is removed before appending so
    /// repeated toggles stay bounded and the most recent state is always last
    /// (e.g. sandbox off -> on -> off yields "on" then "off", not a stale
    /// "off" from the first toggle). Empty input is ignored.
    pub fn queue_event(&mut self, event: &str) {
        if event.is_empty() {
            return;
        }
        let existing = self.pending_event.take().unwrap_or_default();
        let mut next = existing
            .split('\n')
            .filter(|line| !line.is_empty() && *line != event)
            .collect::<Vec<_>>()
            .join("\n");
        if !next.is_empty() {
            next.push('\n');
        }
        next.push_str(event);
        self.pending_event = Some(next);
    }
}

/// Build a one-shot `<event>host changed ...</event>` marker when a thread's
/// stored host differs from the live host it is now running on (issue #679).
/// Returns `None` when either host is unknown/empty or they match, so legacy
/// threads and same-host resumes stay silent.
pub fn host_change_event(old_host: Option<&str>, new_host: Option<&str>) -> Option<String> {
    let old = old_host.map(str::trim).filter(|s| !s.is_empty())?;
    let new = new_host.map(str::trim).filter(|s| !s.is_empty())?;
    // Normalized compare: macOS gethostname() flips between `X` and `X.local`
    // across a network drop, but that is the same machine, so it must not be
    // reported to the LLM as a host switch (issue #683).
    if omnish_common::host::hosts_match(old, new) {
        return None;
    }
    Some(format!(
        "<event>host changed from {} to {}</event>",
        old, new
    ))
}

/// Read a thread's sidecar meta without a `ConversationManager`, so the
/// startup scan can check the soft-delete tombstone before loading (#684).
fn read_meta_at(threads_dir: &std::path::Path, thread_id: &str) -> ThreadMeta {
    let path = threads_dir.join(format!("{}.meta.json", thread_id));
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub struct ConversationManager {
    threads_dir: PathBuf,
    /// In-memory store: thread_id -> raw JSON messages.
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
pub fn sanitize_orphaned_tool_use(msgs: &mut Vec<serde_json::Value>) -> bool {
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
                        "content": "tool execution was interrupted - results unavailable",
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
                        "content": "tool execution was interrupted - results unavailable",
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

/// Text marker appended to merged user messages when continuing after an
/// interrupt. Matches Claude Code's convention so the LLM gets the same
/// structural signal about interrupted tool use.
pub const INTERRUPT_MARKER: &str = "[Request interrupted by user for tool use]";

/// Prefix shared by every interrupt marker; the elapsed-time suffix (issue
/// #682) follows it, so matching must be prefix-based rather than exact.
const INTERRUPT_MARKER_PREFIX: &str = "[Request interrupted by user for tool use";

/// Build the interrupt marker, optionally reporting how long execution ran
/// before the user interrupted (issue #682).
pub fn interrupt_event_message(elapsed: Option<std::time::Duration>) -> String {
    match elapsed {
        Some(d) => format!("{} after {}]", INTERRUPT_MARKER_PREFIX, format_elapsed(d)),
        None => INTERRUPT_MARKER.to_string(),
    }
}

/// True when `text` is an interrupt marker, with or without an elapsed suffix.
pub fn is_interrupt_marker(text: &str) -> bool {
    text.trim_end_matches('\n').starts_with(INTERRUPT_MARKER_PREFIX)
}

/// Compact duration rendering: `45s`, `1m20s`, `1h05m`.
fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// True when the message's role is `user`. Anthropic forbids two consecutive
/// messages with the same role, so any tail that is already user-role forces
/// the next chat turn to merge rather than append.
pub fn is_user_message(msg: &serde_json::Value) -> bool {
    msg["role"].as_str() == Some("user")
}

/// True when a user message's content is a non-empty array of tool_result
/// blocks only (no text blocks yet). These are interrupt snapshots waiting
/// to be merged with the next user query.
fn is_user_tool_result_only(msg: &serde_json::Value) -> bool {
    if !is_user_message(msg) {
        return false;
    }
    let arr = match msg["content"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return false,
    };
    arr.iter().all(|b| b["type"].as_str() == Some("tool_result"))
}

/// Merge a new user query into an existing user-role tail message, in-place.
///
/// Rules:
/// - If `tail` is currently a string content, promote it to an array of text blocks
///   (preserving the original string as the first text block).
/// - If `tail` contains any `tool_result` block and does not already end with the
///   interrupt marker text block, append the marker text block first.
/// - Always append `query` as a final text block.
///
/// `now_ms` timestamps the appended blocks (query and interrupt marker) at the
/// block level, so the new turn is attributed to the current time while any
/// older text already in `tail` keeps its own timestamp (issue #680 review).
/// The message-level `_ts` is left untouched.
///
/// Caller is responsible for persisting the change.
pub fn merge_user_query_into_tail(tail: &mut serde_json::Value, query: &str, now_ms: u64) {
    debug_assert!(is_user_message(tail));
    // Promote string content to array form so we can append blocks uniformly.
    if let Some(s) = tail["content"].as_str() {
        let s = s.to_string();
        let existing_ts = tail.get("_ts").cloned();
        let mut first = serde_json::json!({ "type": "text", "text": s });
        if let Some(ts) = existing_ts {
            first["_ts"] = ts;
        }
        tail["content"] = serde_json::Value::Array(vec![first]);
    }
    let arr = match tail["content"].as_array_mut() {
        Some(a) => a,
        None => return,
    };
    let has_tool_result = arr.iter().any(|b| b["type"].as_str() == Some("tool_result"));
    let already_has_marker = arr.iter().any(|b| {
        b["type"].as_str() == Some("text")
            && b["text"].as_str().is_some_and(is_interrupt_marker)
    });
    if has_tool_result && !already_has_marker {
        arr.push(serde_json::json!({
            "type": "text",
            "text": format!("{}\n", INTERRUPT_MARKER),
            "_ts": now_ms,
        }));
    }
    arr.push(serde_json::json!({
        "type": "text",
        "text": query,
        "_ts": now_ms,
    }));
}

/// True when an assistant message's content is exactly `<event>user interrupted</event>`,
/// either as a plain string or as a single text block. This is the pre-merge
/// format from older builds that we collapse into the preceding user message.
fn is_interrupt_event_marker(msg: &serde_json::Value) -> bool {
    if msg["role"].as_str() != Some("assistant") {
        return false;
    }
    const MARKER: &str = "<event>user interrupted</event>";
    if msg["content"].as_str() == Some(MARKER) {
        return true;
    }
    if let Some(arr) = msg["content"].as_array() {
        return arr.len() == 1
            && arr[0]["type"].as_str() == Some("text")
            && arr[0]["text"].as_str() == Some(MARKER);
    }
    false
}

/// Extract text from a user message's string-or-array content. Returns Some
/// only when the content is a bare string or a single text block (i.e. a plain
/// text user message with no tool_result). For merged user messages the caller
/// should iterate content blocks directly.
fn plain_user_text(msg: &serde_json::Value) -> Option<String> {
    if msg["role"].as_str() != Some("user") {
        return None;
    }
    if let Some(s) = msg["content"].as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = msg["content"].as_array() {
        if arr.len() == 1 && arr[0]["type"].as_str() == Some("text") {
            return arr[0]["text"].as_str().map(|s| s.to_string());
        }
    }
    None
}

/// Collapse the legacy interrupt pattern
/// `user[tool_result*] → assistant[<event>user interrupted</event>] → user[text]`
/// into a single merged `user` message whose content array holds the
/// tool_results plus two text blocks (the interrupt marker and the user's
/// continuation text). Returns true if any changes were made.
///
/// Also handles the dangling case (no following user query yet) by just
/// dropping the orphan `<event>` assistant so the next chat turn can do the
/// forward-path merge cleanly.
pub fn merge_interrupt_markers(msgs: &mut Vec<serde_json::Value>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < msgs.len() {
        if !is_interrupt_event_marker(&msgs[i]) {
            i += 1;
            continue;
        }

        let prev_is_tool_result = i > 0 && is_user_tool_result_only(&msgs[i - 1]);
        let next_text = if i + 1 < msgs.len() {
            plain_user_text(&msgs[i + 1])
        } else {
            None
        };

        match (prev_is_tool_result, next_text) {
            (true, Some(user_text)) => {
                // Merge: append marker + user text as two text blocks to the
                // preceding user[tool_result] message. Drop the <event> and
                // the separate user message.
                if let Some(arr) = msgs[i - 1]["content"].as_array_mut() {
                    arr.push(serde_json::json!({
                        "type": "text",
                        "text": format!("{}\n", INTERRUPT_MARKER),
                    }));
                    arr.push(serde_json::json!({
                        "type": "text",
                        "text": user_text,
                    }));
                }
                msgs.remove(i + 1);
                msgs.remove(i);
                changed = true;
                // i now points to the merged message; advance past it
                i = i.saturating_add(1).min(msgs.len());
            }
            _ => {
                // No adjacent user continuation - just drop the orphan marker.
                msgs.remove(i);
                changed = true;
            }
        }
    }
    changed
}

impl ConversationManager {
    pub fn new(threads_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&threads_dir).ok();

        // Load all existing threads from disk, skipping soft-deleted ones so
        // they stay out of memory and thus out of every listing/resume path
        // (issue #684). Their files are kept for listing and restore.
        let mut threads = HashMap::new();
        let mut skipped_deleted = 0usize;
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
                if read_meta_at(&threads_dir, &thread_id).is_deleted() {
                    skipped_deleted += 1;
                    continue;
                }
                let msgs = match Self::load_thread_file(&threads_dir, &thread_id) {
                    Some(m) => m,
                    None => continue,
                };
                threads.insert(thread_id, msgs);
            }
        }
        tracing::info!(
            "Loaded {} conversation threads from disk ({} soft-deleted skipped)",
            threads.len(),
            skipped_deleted
        );

        Self {
            threads_dir,
            threads: Mutex::new(threads),
        }
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

    /// Atomic read-modify-write for thread meta. Load fresh meta from disk,
    /// apply `f`, then persist. Prefer this over `load_meta` + mutate +
    /// `save_meta` whenever the mutation is based on a snapshot older than
    /// the call, since any fields the caller did not touch are preserved
    /// from the current on-disk state. Notably used by `thread_summary`,
    /// whose LLM calls can race with user commands that mutate unrelated
    /// fields (e.g. `sandbox_disabled`). See issue #587.
    pub fn update_meta<F: FnOnce(&mut ThreadMeta)>(&self, thread_id: &str, f: F) {
        let mut meta = self.load_meta(thread_id);
        f(&mut meta);
        self.save_meta(thread_id, &meta);
    }

    /// Set per-thread sandbox override and persist. `disabled=true` sets the
    /// override; `disabled=false` clears it (back to default "sandbox on").
    pub fn set_sandbox_disabled(&self, thread_id: &str, disabled: bool) {
        self.update_meta(thread_id, |m| {
            m.sandbox_disabled = if disabled { Some(true) } else { None };
            // Queue a marker so the next user message tells the LLM the sandbox
            // state changed (issue #659). Merged with any other pending event
            // (e.g. a host switch queued on resume) rather than clobbering it.
            m.queue_event(&format!(
                "<event>sandbox {}</event>",
                if disabled { "off" } else { "on" }
            ));
        });
    }

    /// Set or clear the user-specified thread title override and persist.
    /// Pass `None` to clear, which lets auto-generated `title_word` take over.
    pub fn set_title_override(&self, thread_id: &str, value: Option<String>) {
        self.update_meta(thread_id, |m| {
            m.title_override = value;
        });
    }

    /// Load thread metadata from the sidecar `.meta.json` file.
    pub fn load_meta(&self, thread_id: &str) -> ThreadMeta {
        read_meta_at(&self.threads_dir, thread_id)
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
        // This scans the directory rather than the in-memory map, so it must
        // skip soft-deleted threads explicitly (issue #684).
        entries.into_iter().find_map(|e| {
            let tid = e.path().file_stem()?.to_string_lossy().to_string();
            if read_meta_at(&self.threads_dir, &tid).is_deleted() {
                return None;
            }
            Some(tid)
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

    /// Soft-delete a thread by ID (issue #684): drop it from memory and stamp
    /// `deleted_at` on its meta, keeping the JSONL, meta and any precompact
    /// snapshots on disk. Dropping it from memory is what hides it, since every
    /// listing/resume path reads the in-memory map. Returns true if the thread
    /// was live and is now tombstoned.
    ///
    /// Keeping the meta also closes the "zombie meta" race: a late `update_meta`
    /// (in-flight usage write, background summary task) can no longer resurrect
    /// a meta file with null host/cwd for a thread that no longer exists.
    pub fn delete_thread(&self, thread_id: &str) -> bool {
        let removed = self.threads.lock().unwrap().remove(thread_id).is_some();
        if removed {
            let now = Self::now_ms();
            self.update_meta(thread_id, |m| m.deleted_at = Some(now));
        }
        removed
    }

    /// Permanently remove a thread's files. Unlike `delete_thread` this does not
    /// require the thread to be live, so an already-tombstoned thread can be
    /// purged. Returns true if anything was removed.
    pub fn purge_thread(&self, thread_id: &str) -> bool {
        self.threads.lock().unwrap().remove(thread_id);
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let meta_path = self.threads_dir.join(format!("{}.meta.json", thread_id));
        let removed = std::fs::remove_file(&path).is_ok() | std::fs::remove_file(&meta_path).is_ok();
        // Remove precompact snapshots belonging to this thread
        let snap_dir = self.threads_dir.join("precompact");
        if let Ok(rd) = std::fs::read_dir(&snap_dir) {
            let prefix = format!("{}.", thread_id);
            for entry in rd.filter_map(|e| e.ok()) {
                if entry.file_name().to_string_lossy().starts_with(&prefix) {
                    std::fs::remove_file(entry.path()).ok();
                }
            }
        }
        removed
    }

    /// Restore a soft-deleted thread: clear the tombstone and load its messages
    /// back into memory so it reappears in listings and can be resumed.
    /// Returns false when the thread is unknown or was not deleted.
    pub fn restore_thread(&self, thread_id: &str) -> bool {
        if !read_meta_at(&self.threads_dir, thread_id).is_deleted() {
            return false;
        }
        let msgs = match Self::load_thread_file(&self.threads_dir, thread_id) {
            Some(m) => m,
            None => return false,
        };
        self.update_meta(thread_id, |m| m.deleted_at = None);
        self.threads
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), msgs);
        true
    }

    /// List soft-deleted threads, newest deletion first, as
    /// `(thread_id, deleted_at_ms, exchange_count, last_question)`.
    ///
    /// Tombstoned threads are deliberately not in memory, so this reads them
    /// from disk on demand. That is fine for an explicit, low-frequency command.
    pub fn list_deleted_conversations(&self) -> Vec<(String, u64, u32, String)> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.threads_dir) {
            Ok(e) => e,
            Err(_) => return out,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let thread_id = match path.file_stem() {
                Some(s) => s.to_string_lossy().to_string(),
                None => continue,
            };
            let deleted_at = match read_meta_at(&self.threads_dir, &thread_id).deleted_at {
                Some(t) => t,
                None => continue,
            };
            let msgs: Vec<serde_json::Value> = std::fs::read_to_string(&path)
                .ok()
                .map(|c| {
                    c.lines()
                        .filter(|l| !l.is_empty())
                        .filter_map(|l| serde_json::from_str(l).ok())
                        .collect()
                })
                .unwrap_or_default();
            let exchange_count = msgs.iter().filter(|m| Self::is_user_input(m)).count() as u32;
            let last_question = msgs
                .iter()
                .rev()
                .find(|m| Self::is_user_input(m))
                .map(Self::extract_text)
                .unwrap_or_default();
            out.push((thread_id, deleted_at, exchange_count, last_question));
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.1));
        out
    }

    /// Current epoch time in milliseconds. Used to timestamp stored messages so
    /// summaries can restrict conversations to a time window (issue #680).
    pub fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Force a standalone message's `_ts` to `ts_ms`. Used for a freshly
    /// appended user message (which `replace_messages` would not stamp).
    ///
    /// Do NOT call this on a tail that `merge_user_query_into_tail` will reuse:
    /// the tail may already hold text from an earlier turn, and re-dating the
    /// whole message would pull that older text into the current window. That
    /// path stamps the appended query/marker blocks instead (issue #680 review).
    pub fn stamp_message_timestamp(msg: &mut serde_json::Value, ts_ms: u64) {
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("_ts".to_string(), serde_json::json!(ts_ms));
        }
    }

    /// Stamp `_ts` (epoch ms) onto every message that lacks one, in place, so
    /// `collect_recent_conversations_md` can filter by time. Existing stamps are
    /// preserved across rewrites (compact, merge, sanitize).
    ///
    /// Known limitation (issue #680 review): assistant/tool messages are only
    /// stamped when they are persisted by `persist_unsaved`, i.e. at the end of
    /// the agent turn, not when the turn started. A turn spanning midnight
    /// (question 23:58, answer 00:03) therefore dates the answer to the next
    /// day, so the two days' summaries each see half the exchange. Accepted:
    /// the alternative (recording the turn start in `AgentLoopState`) is not
    /// worth the plumbing for this edge case.
    fn stamp_missing_timestamps(msgs: &mut [serde_json::Value]) {
        let ts = Self::now_ms();
        for msg in msgs.iter_mut() {
            if let Some(obj) = msg.as_object_mut() {
                obj.entry("_ts").or_insert_with(|| serde_json::json!(ts));
            }
        }
    }

    /// Append raw JSON messages. Writes to both memory and disk (append-only).
    pub fn append_messages(&self, thread_id: &str, messages: &[serde_json::Value]) {
        let mut stamped = messages.to_vec();
        Self::stamp_missing_timestamps(&mut stamped);

        self.threads.lock().unwrap()
            .entry(thread_id.to_string())
            .or_default()
            .extend(stamped.iter().cloned());

        use std::io::Write;
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path)
        {
            for msg in &stamped {
                writeln!(file, "{}", serde_json::to_string(msg).unwrap()).ok();
            }
        }
    }

    /// Read a thread's JSONL from disk, repairing legacy/interrupted structure
    /// in place. Returns `None` when the file cannot be read. Shared by startup
    /// loading and restore so both apply identical normalization.
    fn load_thread_file(
        threads_dir: &std::path::Path,
        thread_id: &str,
    ) -> Option<Vec<serde_json::Value>> {
        let path = threads_dir.join(format!("{}.jsonl", thread_id));
        let content = std::fs::read_to_string(&path).ok()?;
        let mut msgs: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let mut dirty = false;
        if sanitize_orphaned_tool_use(&mut msgs) {
            tracing::warn!("Sanitized orphaned tool_use blocks in thread {}", thread_id);
            dirty = true;
        }
        if merge_interrupt_markers(&mut msgs) {
            tracing::warn!("Merged legacy interrupt markers in thread {}", thread_id);
            dirty = true;
        }
        if dirty {
            Self::rewrite_thread_file(threads_dir, thread_id, &msgs);
        }
        Some(msgs)
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

    /// Load the live window of a thread as raw JSON for LLM context: every
    /// message from the last full-compaction summary onward (issue #687), or
    /// the whole thread when it was never fully compacted. Older messages stay
    /// on disk and in memory but are no longer part of the context.
    pub fn load_raw_messages(&self, thread_id: &str) -> Vec<serde_json::Value> {
        let threads = self.threads.lock().unwrap();
        let msgs = match threads.get(thread_id) {
            Some(m) => m,
            None => return Vec::new(),
        };
        msgs[crate::compact::compact_boundary(msgs)..].to_vec()
    }

    /// Replace the live window of a thread (see `load_raw_messages`) and
    /// rewrite its JSONL file. Messages before the last compaction summary are
    /// kept as-is, so callers can round-trip `load_raw_messages` output without
    /// dropping compacted history.
    ///
    /// Unlike `append_messages`, this does NOT stamp missing timestamps: the
    /// list usually comes from already-stored history plus a caller-stamped new
    /// turn, and blanket-stamping would date legacy messages as "now" and leak
    /// them into windowed summaries (issue #680).
    pub fn replace_messages(&self, thread_id: &str, msgs: &[serde_json::Value]) {
        let mut threads = self.threads.lock().unwrap();
        let mut all = match threads.get(thread_id) {
            Some(old) => old[..crate::compact::compact_boundary(old)].to_vec(),
            None => Vec::new(),
        };
        all.extend_from_slice(msgs);
        Self::rewrite_thread_file(&self.threads_dir, thread_id, &all);
        threads.insert(thread_id.to_string(), all);
    }

    /// Live window plus the total message count, taken under one lock. The
    /// count is the guard token for `append_compact_summary`.
    pub fn snapshot_for_compact(&self, thread_id: &str) -> Option<(Vec<serde_json::Value>, usize)> {
        let threads = self.threads.lock().unwrap();
        let msgs = threads.get(thread_id)?;
        Some((msgs[crate::compact::compact_boundary(msgs)..].to_vec(), msgs.len()))
    }

    /// Append a full-compaction summary message (issue #687), unless the thread
    /// grew since `snapshot_for_compact` returned `expected_len`: the summary
    /// would then silently hide the newer messages from the context.
    pub fn append_compact_summary(
        &self,
        thread_id: &str,
        expected_len: usize,
        msg: serde_json::Value,
    ) -> Result<(), String> {
        let mut threads = self.threads.lock().unwrap();
        let msgs = threads
            .get_mut(thread_id)
            .ok_or_else(|| "thread not found".to_string())?;
        if msgs.len() != expected_len {
            return Err("thread changed during compaction, try again".to_string());
        }
        use std::io::Write;
        let path = self.threads_dir.join(format!("{}.jsonl", thread_id));
        let mut file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path)
            .map_err(|e| format!("open thread file failed: {}", e))?;
        writeln!(file, "{}", serde_json::to_string(&msg).unwrap())
            .map_err(|e| format!("write thread file failed: {}", e))?;
        msgs.push(msg);
        Ok(())
    }

    /// Compact bulky tool_result blocks in a thread (issue #641, `/compact mini`).
    /// Only the live window is touched; history before the last full
    /// compaction summary is left verbatim.
    ///
    /// Snapshots the current JSONL to `precompact/{tid}.{unix_ts}.jsonl`
    /// before the destructive rewrite; a failed snapshot aborts the rewrite.
    /// Returns Ok(None) when there is nothing to compact.
    pub fn compact_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<crate::compact::CompactStats>, String> {
        let mut threads = self.threads.lock().unwrap();
        let msgs = threads
            .get_mut(thread_id)
            .ok_or_else(|| "thread not found".to_string())?;

        let mut candidate = msgs.clone();
        let boundary = crate::compact::compact_boundary(&candidate);
        let stats = crate::compact::compact_messages(&mut candidate[boundary..]);
        if stats.blocks == 0 {
            return Ok(None);
        }

        let snap_dir = self.threads_dir.join("precompact");
        std::fs::create_dir_all(&snap_dir)
            .map_err(|e| format!("snapshot dir failed: {}", e))?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let src = self.threads_dir.join(format!("{}.jsonl", thread_id));
        // Same-second collisions are harmless: a second compaction finds nothing new and skips.
        let snap = snap_dir.join(format!("{}.{}.jsonl", thread_id, ts));
        std::fs::copy(&src, &snap).map_err(|e| format!("snapshot failed: {}", e))?;

        *msgs = candidate;
        Self::rewrite_thread_file(&self.threads_dir, thread_id, msgs);
        tracing::info!(
            "Compacted thread {}: {} blocks, {} -> {} chars (snapshot: {})",
            thread_id, stats.blocks, stats.chars_before, stats.chars_after,
            snap.display()
        );
        Ok(Some(stats))
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
    /// A full-compaction summary counts only once a user query was merged into it.
    fn is_user_input(msg: &serde_json::Value) -> bool {
        if crate::compact::is_compact_message(msg) {
            return crate::compact::compact_has_query(msg);
        }
        msg["role"].as_str() == Some("user") && msg["content"].is_string()
    }

    /// Extract only the text that falls inside a time window. A text block's own
    /// `_ts` wins; otherwise the message-level `_ts` applies. For string content
    /// the message-level `_ts` decides. Used by windowed summaries so a merged
    /// turn contributes each part to the right day (issue #680 review).
    fn extract_text_in_window(msg: &serde_json::Value, cutoff_ms: u64) -> String {
        let msg_ts = msg.get("_ts").and_then(|v| v.as_u64()).unwrap_or(0);
        match &msg["content"] {
            serde_json::Value::String(s) => {
                if msg_ts >= cutoff_ms { s.clone() } else { String::new() }
            }
            serde_json::Value::Array(arr) => arr
                .iter()
                .enumerate()
                .filter_map(|(idx, b)| {
                    if b["type"].as_str() != Some("text")
                        || crate::compact::is_compact_summary_block(msg, idx)
                    {
                        return None;
                    }
                    let ts = b.get("_ts").and_then(|v| v.as_u64()).unwrap_or(msg_ts);
                    if ts < cutoff_ms {
                        return None;
                    }
                    b["text"].as_str().map(|s| s.to_string())
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    /// Extract display text from a message, stripping <system-reminder> blocks.
    /// The summary block of a full-compaction message is skipped, leaving only
    /// any user query merged into it.
    fn extract_text(msg: &serde_json::Value) -> String {
        match &msg["content"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => {
                arr.iter()
                    .enumerate()
                    .filter_map(|(idx, b)| {
                        if b["type"].as_str() == Some("text")
                            && !crate::compact::is_compact_summary_block(msg, idx)
                        {
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
    ///
    /// Only text stamped `_ts >= since` is included, so a long-running thread
    /// contributes just the turns from this window instead of its whole history
    /// (issue #680). For merged user turns the filter is per text block, so the
    /// new query goes to the current day while older text in the same message
    /// stays in its own day. Messages/blocks without a `_ts` predate this
    /// tracking and are treated as out of window.
    pub fn collect_recent_conversations_md(&self, since: std::time::SystemTime) -> String {
        let cutoff_ms = since
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
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

            let mut body = String::new();
            for msg in &messages {
                let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                let text = Self::extract_text_in_window(msg, cutoff_ms);
                if text.is_empty() {
                    continue;
                }
                let label = match role {
                    "user" => "User",
                    "assistant" => "Assistant",
                    _ => continue,
                };
                body.push_str(&format!("{}: {}\n\n", label, text));
            }

            if !body.is_empty() {
                result.push_str(&format!("## Thread: {}\n\n{}", title, body));
            }
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
    fn test_append_messages_stamps_timestamp() {
        // Issue #680: new messages carry a `_ts` so summaries can window them.
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        mgr.append_messages(&id, &[user_msg("hi"), assistant_msg("hello")]);
        let msgs = mgr.load_raw_messages(&id);
        assert!(msgs[0].get("_ts").and_then(|v| v.as_u64()).is_some());
        assert!(msgs[1].get("_ts").and_then(|v| v.as_u64()).is_some());
    }

    #[test]
    fn test_collect_recent_conversations_filters_by_message_ts() {
        // A long-running thread must contribute only the turns inside the
        // window, not its whole history (issue #680).
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        let old_ms = ConversationManager::now_ms() - 48 * 3600 * 1000;
        let mut old_user = user_msg("old question");
        ConversationManager::stamp_message_timestamp(&mut old_user, old_ms);
        let mut old_assistant = assistant_msg("old answer");
        ConversationManager::stamp_message_timestamp(&mut old_assistant, old_ms);
        mgr.append_messages(&id, &[old_user, old_assistant]);

        // New turn, stamped "now" by append_messages.
        mgr.append_messages(&id, &[user_msg("new question"), assistant_msg("new answer")]);

        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let md = mgr.collect_recent_conversations_md(since);
        assert!(md.contains("new question"), "md: {}", md);
        assert!(md.contains("new answer"), "md: {}", md);
        assert!(!md.contains("old question"), "md: {}", md);
        assert!(!md.contains("old answer"), "md: {}", md);
    }

    #[test]
    fn test_merged_turn_windows_per_block() {
        // Issue #680 review: a query merged into an old user tail is attributed
        // to the current window, while the older text stays in its own day.
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        let old_ms = ConversationManager::now_ms() - 48 * 3600 * 1000;
        let now_ms = ConversationManager::now_ms();
        let mut tail = user_msg("old question");
        ConversationManager::stamp_message_timestamp(&mut tail, old_ms);
        merge_user_query_into_tail(&mut tail, "new question", now_ms);
        mgr.replace_messages(&id, &[tail]);

        let recent = mgr.collect_recent_conversations_md(
            std::time::SystemTime::now() - std::time::Duration::from_secs(3600),
        );
        assert!(recent.contains("new question"), "recent: {}", recent);
        assert!(!recent.contains("old question"), "recent: {}", recent);

        let all = mgr.collect_recent_conversations_md(
            std::time::SystemTime::now() - std::time::Duration::from_secs(72 * 3600),
        );
        assert!(all.contains("new question"), "all: {}", all);
        assert!(all.contains("old question"), "all: {}", all);
    }

    #[test]
    fn test_collect_recent_skips_undated_legacy_messages() {
        // Legacy messages without `_ts` cannot be dated; they must not leak
        // into a windowed summary. replace_messages does not stamp them.
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        mgr.replace_messages(&id, &[user_msg("legacy"), assistant_msg("old")]);
        assert!(mgr.load_raw_messages(&id)[0].get("_ts").is_none());

        let since = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let md = mgr.collect_recent_conversations_md(since);
        assert!(!md.contains("legacy"), "md: {}", md);
        assert!(!md.contains("old"), "md: {}", md);
    }

    #[test]
    fn test_replace_messages_preserves_existing_ts() {
        // Rewrites (compact/merge/sanitize) must not re-date stored messages.
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());

        let ts = ConversationManager::now_ms() - 48 * 3600 * 1000;
        let mut m = user_msg("keep");
        ConversationManager::stamp_message_timestamp(&mut m, ts);
        mgr.replace_messages(&id, &[m]);

        assert_eq!(mgr.load_raw_messages(&id)[0]["_ts"].as_u64(), Some(ts));
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

        // system-reminder is NOT stored - server strips it before persisting
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

        // Delete first thread (soft delete, issue #684)
        assert!(mgr.delete_thread(&id1));
        // Memory: dropped, so every listing/resume path stops seeing it
        assert!(mgr.load_raw_messages(&id1).is_empty());
        assert!(!mgr.thread_exists(&id1));
        assert!(!mgr.list_conversations().iter().any(|(t, ..)| *t == id1));
        // Disk: the JSONL is kept and the meta carries the tombstone
        let path1 = dir.path().join(format!("{}.jsonl", id1));
        assert!(path1.exists(), "soft delete must keep the conversation file");
        assert!(mgr.load_meta(&id1).is_deleted());
        // Second thread still exists
        assert_eq!(mgr.load_raw_messages(&id2).len(), 1);

        // Deleting an already-deleted thread returns false
        assert!(!mgr.delete_thread(&id1));
    }

    /// Issue #684: a soft-deleted thread can be listed and brought back.
    #[test]
    fn test_restore_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("hello")]);
        assert!(mgr.delete_thread(&id));

        // Listed as deleted, with its content still readable from disk.
        let deleted = mgr.list_deleted_conversations();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, id);
        assert_eq!(deleted[0].2, 1, "exchange count read from disk");

        // Restore brings it back into memory and clears the tombstone.
        assert!(mgr.restore_thread(&id));
        assert!(!mgr.load_meta(&id).is_deleted());
        assert_eq!(mgr.load_raw_messages(&id).len(), 1);
        assert!(mgr.thread_exists(&id));
        assert!(mgr.list_deleted_conversations().is_empty());

        // Restoring a live thread is a no-op.
        assert!(!mgr.restore_thread(&id));
    }

    /// Issue #684: tombstoned threads must not be loaded on restart, and must
    /// stay invisible to listing and "resume latest".
    #[test]
    fn test_deleted_thread_skipped_on_reload() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, gone) = {
            let mgr = ConversationManager::new(dir.path().to_path_buf());
            let kept = mgr.create_thread(ThreadMeta::default());
            let gone = mgr.create_thread(ThreadMeta::default());
            mgr.append_messages(&kept, &[user_msg("keep")]);
            mgr.append_messages(&gone, &[user_msg("drop")]);
            assert!(mgr.delete_thread(&gone));
            (kept, gone)
        };

        // Fresh manager = daemon restart.
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.thread_exists(&kept));
        assert!(!mgr.thread_exists(&gone), "tombstoned thread must not load");
        assert!(!mgr.list_conversations().iter().any(|(t, ..)| *t == gone));
        // get_latest_thread scans the directory, so it needs its own guard.
        assert_eq!(mgr.get_latest_thread().as_deref(), Some(kept.as_str()));
        // Still restorable after the restart.
        assert!(mgr.restore_thread(&gone));
        assert!(mgr.thread_exists(&gone));
    }

    /// Issue #684: purge is the hard delete, and works on a tombstoned thread
    /// (which `delete_thread` alone will not touch, since it is not in memory).
    #[test]
    fn test_purge_thread_removes_files() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("x")]);
        assert!(mgr.delete_thread(&id));

        assert!(mgr.purge_thread(&id));
        assert!(!dir.path().join(format!("{}.jsonl", id)).exists());
        assert!(!dir.path().join(format!("{}.meta.json", id)).exists());
        assert!(mgr.list_deleted_conversations().is_empty());
        assert!(!mgr.restore_thread(&id));
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

        // Soft delete keeps the meta (that is where the tombstone lives, and
        // keeping it is what prevents a late update_meta from resurrecting a
        // null host/cwd zombie). Purge is what removes it. Issue #684.
        mgr.delete_thread(&id);
        assert!(meta_path.exists());
        let meta = mgr.load_meta(&id);
        assert!(meta.is_deleted());
        assert_eq!(meta.host.as_deref(), Some("host"), "fields survive delete");

        mgr.purge_thread(&id);
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
    fn test_set_sandbox_disabled_queues_pending_event() {
        // Issue #659: toggling sandbox queues a one-shot marker for the next
        // user message. Repeated toggles are newline-joined (issue #679 review)
        // rather than last-write-wins, so an interleaved host event survives.
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        let tid = mgr.create_thread(ThreadMeta::default());
        assert_eq!(mgr.load_meta(&tid).pending_event, None);

        mgr.set_sandbox_disabled(&tid, true);
        assert_eq!(
            mgr.load_meta(&tid).pending_event.as_deref(),
            Some("<event>sandbox off</event>")
        );

        // Toggle back before the marker is consumed: both markers are kept.
        mgr.set_sandbox_disabled(&tid, false);
        assert_eq!(
            mgr.load_meta(&tid).pending_event.as_deref(),
            Some("<event>sandbox off</event>\n<event>sandbox on</event>")
        );
    }

    #[test]
    fn test_host_change_event() {
        // Issue #679: resuming a thread on a different host queues a marker.
        assert_eq!(
            host_change_event(Some("a"), Some("b")).as_deref(),
            Some("<event>host changed from a to b</event>")
        );
        // Same host, unknown, or empty sides stay silent (no malformed marker).
        assert_eq!(host_change_event(Some("a"), Some("a")), None);
        assert_eq!(host_change_event(None, Some("b")), None);
        assert_eq!(host_change_event(Some("a"), None), None);
        assert_eq!(host_change_event(Some("a"), Some("  ")), None);
        assert_eq!(host_change_event(Some(""), Some("b")), None);
        assert_eq!(host_change_event(None, None), None);
        // Issue #683: the macOS `.local` flip is the same machine, so it must
        // not be reported as a host switch.
        assert_eq!(host_change_event(Some("mac"), Some("mac.local")), None);
        assert_eq!(host_change_event(Some("mac.local"), Some("mac")), None);
        assert_eq!(host_change_event(Some("mac.LOCAL"), Some("MAC")), None);
        // A genuinely different machine still reports.
        assert!(host_change_event(Some("mac.local"), Some("titan")).is_some());
    }

    #[test]
    fn test_queue_event_merges_and_dedups() {
        let mut meta = ThreadMeta::default();
        // Empty input is a no-op.
        meta.queue_event("");
        assert_eq!(meta.pending_event, None);

        meta.queue_event("<event>host changed from a to b</event>");
        meta.queue_event("<event>sandbox off</event>");
        let merged = "<event>host changed from a to b</event>\n<event>sandbox off</event>";
        assert_eq!(meta.pending_event.as_deref(), Some(merged));

        // Re-queueing an identical marker does not accumulate duplicates.
        meta.queue_event("<event>sandbox off</event>");
        assert_eq!(meta.pending_event.as_deref(), Some(merged));
    }

    #[test]
    fn test_queue_event_moves_repeat_to_end() {
        // Issue #679 review: the most recent state must be last. off -> on -> off
        // must not leave the stale first "off" line as the duplicate that wins.
        let mut meta = ThreadMeta::default();
        meta.queue_event("<event>sandbox off</event>");
        meta.queue_event("<event>sandbox on</event>");
        meta.queue_event("<event>sandbox off</event>");
        assert_eq!(
            meta.pending_event.as_deref(),
            Some("<event>sandbox on</event>\n<event>sandbox off</event>")
        );
    }

    #[test]
    fn test_sandbox_toggle_preserves_pending_host_event() {
        // Issue #679 review: a host marker queued on resume must survive a
        // later `/thread sandbox off` (the reverse order used to clobber it).
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let tid = mgr.create_thread(ThreadMeta::default());

        mgr.update_meta(&tid, |m| {
            m.queue_event("<event>host changed from a to b</event>");
        });
        mgr.set_sandbox_disabled(&tid, true);
        assert_eq!(
            mgr.load_meta(&tid).pending_event.as_deref(),
            Some("<event>host changed from a to b</event>\n<event>sandbox off</event>")
        );
    }

    #[test]
    fn test_update_meta_preserves_concurrent_field_changes() {
        // Issue #587 regression: thread_summary used to load meta, do async
        // LLM work, then save its stale clone. Any concurrent mutation to
        // unrelated fields (e.g. sandbox_disabled) during the LLM window
        // was clobbered. update_meta re-reads before save, preserving
        // fields the caller did not touch.
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());

        let tid = mgr.create_thread(ThreadMeta::default());

        // Simulate thread_summary taking a snapshot (stale from now on).
        let _stale_snapshot = mgr.load_meta(&tid);

        // Concurrent user action: /thread sandbox off.
        mgr.set_sandbox_disabled(&tid, true);
        assert_eq!(mgr.load_meta(&tid).sandbox_disabled, Some(true));

        // thread_summary finishes LLM work and writes its owned fields.
        // With the pre-fix `save_meta(&stale_snapshot)`, sandbox_disabled
        // would revert to None. update_meta must preserve it.
        mgr.update_meta(&tid, |m| {
            m.summary = Some("generated summary".to_string());
            m.summary_rounds = Some(1);
            m.title_word = Some("acknowledgment".to_string());
        });

        let after = mgr.load_meta(&tid);
        assert_eq!(after.sandbox_disabled, Some(true), "user change must survive");
        assert_eq!(after.summary.as_deref(), Some("generated summary"));
        assert_eq!(after.summary_rounds, Some(1));
        assert_eq!(after.title_word.as_deref(), Some("acknowledgment"));
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

    fn interrupt_event_string() -> serde_json::Value {
        serde_json::json!({
            "role": "assistant",
            "content": "<event>user interrupted</event>"
        })
    }

    fn interrupt_event_array() -> serde_json::Value {
        serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "<event>user interrupted</event>"}
            ]
        })
    }

    fn tool_result_only_msg(tool_use_id: &str, content: &str, is_error: bool) -> serde_json::Value {
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error}
            ]
        })
    }

    #[test]
    fn test_merge_interrupt_markers_single_tool() {
        // user[tool_result(interrupted)] → assistant[<event>] → user[text]
        // collapses into a single user message with [tool_result, text, text].
        let mut msgs = vec![
            user_msg("run something"),
            assistant_with_tool_use(),
            tool_result_only_msg("toolu_1", "user interrupted", true),
            interrupt_event_string(),
            user_msg("retry please"),
        ];
        assert!(merge_interrupt_markers(&mut msgs));
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "user");
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], format!("{}\n", INTERRUPT_MARKER));
        assert_eq!(content[2]["type"], "text");
        assert_eq!(content[2]["text"], "retry please");
    }

    #[test]
    fn test_merge_interrupt_markers_array_form() {
        // The event marker can also appear as a single-text-block array
        // (happens when cache_control rewrote the message content).
        let mut msgs = vec![
            tool_result_only_msg("toolu_1", "user interrupted", true),
            interrupt_event_array(),
            user_msg("go on"),
        ];
        assert!(merge_interrupt_markers(&mut msgs));
        assert_eq!(msgs.len(), 1);
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[2]["text"], "go on");
    }

    #[test]
    fn test_merge_interrupt_markers_orphan_event() {
        // assistant[<event>] not between tool_result and user[text] → just drop it.
        let mut msgs = vec![
            user_msg("hi"),
            assistant_msg("hello"),
            interrupt_event_string(),
        ];
        assert!(merge_interrupt_markers(&mut msgs));
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_merge_interrupt_markers_multiple_interrupts() {
        // Two interrupt sequences in the same thread - both get merged.
        let mut msgs = vec![
            user_msg("first"),
            assistant_with_tool_use(),
            tool_result_only_msg("toolu_1", "user interrupted", true),
            interrupt_event_string(),
            user_msg("retry 1"),
            assistant_with_tool_use(),
            tool_result_only_msg("toolu_1", "user interrupted", true),
            interrupt_event_string(),
            user_msg("retry 2"),
        ];
        assert!(merge_interrupt_markers(&mut msgs));
        assert_eq!(msgs.len(), 5);
        // Both merged messages should contain the marker + user text
        for idx in [2usize, 4usize] {
            let content = msgs[idx]["content"].as_array().unwrap();
            assert_eq!(content.len(), 3);
            assert_eq!(content[0]["type"], "tool_result");
            assert_eq!(content[1]["type"], "text");
            assert_eq!(content[2]["type"], "text");
        }
        assert_eq!(msgs[2]["content"].as_array().unwrap()[2]["text"], "retry 1");
        assert_eq!(msgs[4]["content"].as_array().unwrap()[2]["text"], "retry 2");
    }

    #[test]
    fn test_merge_interrupt_markers_no_change_when_absent() {
        // Normal conversation with no interrupts - no modifications.
        let mut msgs = vec![
            user_msg("do something"),
            assistant_with_tool_use(),
            tool_result_msg(),
            assistant_msg("done"),
        ];
        assert!(!merge_interrupt_markers(&mut msgs));
        assert_eq!(msgs.len(), 4);
    }

    #[test]
    fn test_merge_interrupt_markers_parallel_tool_results() {
        // Parallel case: user message with multiple tool_results, all interrupted.
        let mut msgs = vec![
            user_msg("run three things"),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "t1", "name": "a", "input": {}},
                    {"type": "tool_use", "id": "t2", "name": "b", "input": {}},
                    {"type": "tool_use", "id": "t3", "name": "c", "input": {}},
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "user interrupted", "is_error": true},
                    {"type": "tool_result", "tool_use_id": "t2", "content": "user interrupted", "is_error": true},
                    {"type": "tool_result", "tool_use_id": "t3", "content": "user interrupted", "is_error": true},
                ]
            }),
            interrupt_event_string(),
            user_msg("继续"),
        ];
        assert!(merge_interrupt_markers(&mut msgs));
        assert_eq!(msgs.len(), 3);
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 5);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[3]["type"], "text");
        assert_eq!(content[4]["text"], "继续");
    }

    #[test]
    fn test_is_user_tool_result_only() {
        assert!(is_user_tool_result_only(&tool_result_msg()));
        assert!(!is_user_tool_result_only(&user_msg("hi")));
        assert!(!is_user_tool_result_only(&assistant_msg("x")));
        // Mixed content (has text block too) is not tool_result-only
        let merged = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "x", "is_error": false},
                {"type": "text", "text": "hi"}
            ]
        });
        assert!(!is_user_tool_result_only(&merged));
    }

    #[test]
    fn test_is_user_message() {
        assert!(is_user_message(&user_msg("hi")));
        assert!(is_user_message(&tool_result_msg()));
        assert!(!is_user_message(&assistant_msg("x")));
    }

    #[test]
    fn test_interrupt_event_message_and_is_marker() {
        assert_eq!(interrupt_event_message(None), INTERRUPT_MARKER);
        assert_eq!(
            interrupt_event_message(Some(std::time::Duration::from_secs(45))),
            "[Request interrupted by user for tool use after 45s]"
        );
        assert_eq!(
            interrupt_event_message(Some(std::time::Duration::from_secs(80))),
            "[Request interrupted by user for tool use after 1m20s]"
        );
        assert_eq!(
            interrupt_event_message(Some(std::time::Duration::from_secs(3900))),
            "[Request interrupted by user for tool use after 1h05m]"
        );
        assert!(is_interrupt_marker(INTERRUPT_MARKER));
        assert!(is_interrupt_marker(
            "[Request interrupted by user for tool use after 1m20s]\n"
        ));
        assert!(!is_interrupt_marker("hello"));
    }

    #[test]
    fn test_merge_does_not_duplicate_duration_marker() {
        // An interrupt with an elapsed suffix is already present: merge must
        // recognize it (prefix match) and only append the new query.
        let marker = interrupt_event_message(Some(std::time::Duration::from_secs(80)));
        let mut tail = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "x", "is_error": true},
                {"type": "text", "text": format!("{}\n", marker)},
            ]
        });
        merge_user_query_into_tail(&mut tail, "next", 1);
        let arr = tail["content"].as_array().unwrap();
        let markers = arr
            .iter()
            .filter(|b| {
                b["type"].as_str() == Some("text")
                    && b["text"].as_str().is_some_and(is_interrupt_marker)
            })
            .count();
        assert_eq!(markers, 1);
        assert_eq!(arr.last().unwrap()["text"], "next");
    }

    #[test]
    fn test_merge_user_query_into_tail_tool_result_only() {
        // Fresh interrupt snapshot: tool_result + marker + new query.
        let mut tail = tool_result_only_msg("t1", "user interrupted", true);
        merge_user_query_into_tail(&mut tail, "继续", 111);
        let arr = tail["content"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["type"], "tool_result");
        assert_eq!(arr[1]["type"], "text");
        assert_eq!(arr[1]["text"], format!("{}\n", INTERRUPT_MARKER));
        assert_eq!(arr[2]["text"], "继续");
        // Appended blocks carry the current timestamp.
        assert_eq!(arr[1]["_ts"], 111);
        assert_eq!(arr[2]["_ts"], 111);
    }

    #[test]
    fn test_merge_user_query_into_tail_already_merged() {
        // Second "继续" arriving on top of an already-merged tail must NOT
        // duplicate the marker, only append the new query text.
        let mut tail = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "user interrupted", "is_error": true},
                {"type": "text", "text": format!("{}\n", INTERRUPT_MARKER)},
                {"type": "text", "text": "继续"},
            ]
        });
        merge_user_query_into_tail(&mut tail, "再继续", 222);
        let arr = tail["content"].as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[3]["text"], "再继续");
        assert_eq!(arr[3]["_ts"], 222);
        // Only one marker in the array
        let marker_count = arr.iter()
            .filter(|b| b["type"].as_str() == Some("text")
                && b["text"].as_str().is_some_and(|t| t.trim_end_matches('\n') == INTERRUPT_MARKER))
            .count();
        assert_eq!(marker_count, 1);
    }

    #[test]
    fn test_merge_user_query_into_tail_plain_string() {
        // Tail is a plain user query whose agent got superseded; promote
        // the string to an array and append the new query (no marker since
        // there's no tool_result to mark).
        let mut tail = user_msg("first");
        merge_user_query_into_tail(&mut tail, "second", 333);
        let arr = tail["content"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "first");
        assert_eq!(arr[1]["text"], "second");
        assert_eq!(arr[1]["_ts"], 333);
        // No interrupt marker injected when there's no tool_result context
        let marker_count = arr.iter()
            .filter(|b| b["type"].as_str() == Some("text")
                && b["text"].as_str().is_some_and(|t| t.trim_end_matches('\n') == INTERRUPT_MARKER))
            .count();
        assert_eq!(marker_count, 0);
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
    fn test_title_override_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta {
            title_word: Some("deploy".to_string()),
            ..Default::default()
        });

        // No override: effective label == title_word
        let meta = mgr.load_meta(&id);
        assert_eq!(meta.effective_title_label().as_deref(), Some("deploy"));

        // Set override: precedence over title_word
        mgr.set_title_override(&id, Some("release pipeline".to_string()));
        let meta = mgr.load_meta(&id);
        assert_eq!(meta.title_override.as_deref(), Some("release pipeline"));
        assert_eq!(meta.title_word.as_deref(), Some("deploy"));
        assert_eq!(meta.effective_title_label().as_deref(), Some("release pipeline"));

        // Clear override: title_word takes over again
        mgr.set_title_override(&id, None);
        let meta = mgr.load_meta(&id);
        assert!(meta.title_override.is_none());
        assert_eq!(meta.effective_title_label().as_deref(), Some("deploy"));
    }

    #[test]
    fn test_effective_title_label_when_both_unset() {
        let meta = ThreadMeta::default();
        assert!(meta.effective_title_label().is_none());
    }

    #[test]
    fn test_sanitize_persisted_on_load() {
        // Write an orphaned tool_use to disk, then load - should be sanitized
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
        // Reload from disk - startup sanitization should fix it
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

    fn big_tool_round(id: &str) -> Vec<serde_json::Value> {
        let big = (1..=50).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        vec![
            serde_json::json!({"role": "user", "content": "run it"}),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "bash", "input": {}}
            ]}),
            serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": big}
            ]}),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "text", "text": "done"}
            ]}),
        ]
    }

    fn snapshot_files(dir: &std::path::Path, thread_id: &str) -> Vec<std::path::PathBuf> {
        let snap_dir = dir.join("precompact");
        match std::fs::read_dir(&snap_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(&format!("{}.", thread_id))
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn test_compact_thread_writes_snapshot_and_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        // 3 rounds; the first is outside the protected window
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("tu_{}", r)));
        }
        let original = mgr.load_raw_messages(&id);

        let stats = mgr.compact_thread(&id).unwrap().expect("should compact");
        assert_eq!(stats.blocks, 1);

        // In-memory and on-disk both rewritten
        let after = mgr.load_raw_messages(&id);
        assert_ne!(original, after);
        let compacted_text = after[2]["content"][0]["content"].as_str().unwrap();
        assert!(compacted_text.contains(crate::compact::COMPACT_MARKER));
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        assert_eq!(mgr2.load_raw_messages(&id), after);

        // Snapshot exists and contains the original messages
        let snaps = snapshot_files(dir.path(), &id);
        assert_eq!(snaps.len(), 1);
        let snap_content = std::fs::read_to_string(&snaps[0]).unwrap();
        let snap_msgs: Vec<serde_json::Value> = snap_content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(snap_msgs, original);
    }

    #[test]
    fn test_compact_thread_nothing_to_compact() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("hi"), assistant_msg("hello")]);

        let res = mgr.compact_thread(&id).unwrap();
        assert!(res.is_none());
        // No snapshot written
        assert!(snapshot_files(dir.path(), &id).is_empty());
    }

    #[test]
    fn test_compact_thread_unknown_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        assert!(mgr.compact_thread("nope").is_err());
    }

    #[test]
    fn test_delete_thread_removes_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("tu_{}", r)));
        }
        mgr.compact_thread(&id).unwrap().expect("should compact");
        assert_eq!(snapshot_files(dir.path(), &id).len(), 1);

        // Soft delete keeps snapshots so a restore is complete (#684).
        assert!(mgr.delete_thread(&id));
        assert_eq!(snapshot_files(dir.path(), &id).len(), 1);
        // Purge is the hard delete and removes them.
        assert!(mgr.purge_thread(&id));
        assert!(snapshot_files(dir.path(), &id).is_empty());
    }

    #[test]
    fn test_snapshot_not_listed_as_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("tu_{}", r)));
        }
        mgr.compact_thread(&id).unwrap().expect("should compact");

        // Reload from disk: only the real thread must be discovered
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        let ids = mgr2.list_thread_ids();
        assert_eq!(ids, vec![id]);
    }

    // --- full compaction (issue #687) ---

    fn disk_messages(dir: &std::path::Path, id: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(dir.join(format!("{}.jsonl", id)))
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Append a full-compaction summary the way the daemon command does.
    fn full_compact(mgr: &ConversationManager, id: &str, summary: &str) {
        let (_, total) = mgr.snapshot_for_compact(id).unwrap();
        let msg = crate::compact::make_compact_message(summary, ConversationManager::now_ms());
        mgr.append_compact_summary(id, total, msg).unwrap();
    }

    #[test]
    fn test_full_compact_keeps_log_and_windows_context() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("q1"), assistant_msg("a1"), user_msg("q2"), assistant_msg("a2")]);

        let (window, total) = mgr.snapshot_for_compact(&id).unwrap();
        assert_eq!((window.len(), total), (4, 4));
        full_compact(&mgr, &id, "SUMMARY TEXT");

        // Context starts at the summary; the old log is still on disk.
        let live = mgr.load_raw_messages(&id);
        assert_eq!(live.len(), 1);
        assert!(crate::compact::is_compact_message(&live[0]));
        let disk = disk_messages(dir.path(), &id);
        assert_eq!(disk.len(), 5);
        assert_eq!(disk[0]["content"], "q1");

        // Survives a daemon restart.
        let mgr2 = ConversationManager::new(dir.path().to_path_buf());
        assert_eq!(mgr2.load_raw_messages(&id), live);
    }

    #[test]
    fn test_full_compact_rejects_when_thread_grew() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("q1"), assistant_msg("a1")]);
        let (_, total) = mgr.snapshot_for_compact(&id).unwrap();
        mgr.append_messages(&id, &[user_msg("q2")]);

        let msg = crate::compact::make_compact_message("S", 1);
        assert!(mgr.append_compact_summary(&id, total, msg).is_err());
        assert_eq!(mgr.load_raw_messages(&id).len(), 3);
        assert_eq!(disk_messages(dir.path(), &id).len(), 3);
    }

    #[test]
    fn test_replace_messages_keeps_pre_compact_history() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("q1"), assistant_msg("a1")]);
        full_compact(&mgr, &id, "S");

        // Chat path: merge the next query into the summary tail, rewrite.
        let mut window = mgr.load_raw_messages(&id);
        merge_user_query_into_tail(window.last_mut().unwrap(), "q2", 5);
        mgr.replace_messages(&id, &window);
        mgr.append_messages(&id, &[assistant_msg("a2")]);

        let disk = disk_messages(dir.path(), &id);
        assert_eq!(disk.len(), 4, "q1, a1, summary+q2, a2");
        assert_eq!(disk[0]["content"], "q1");
        assert!(crate::compact::is_compact_message(&disk[2]));
        assert_eq!(mgr.load_raw_messages(&id).len(), 2);
    }

    #[test]
    fn test_rounds_and_text_hide_compact_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        mgr.append_messages(&id, &[user_msg("q1"), assistant_msg("a1")]);
        full_compact(&mgr, &id, "SUMMARY TEXT");
        assert_eq!(mgr.count_rounds(&id), 1, "a bare summary is not a round");

        let mut window = mgr.load_raw_messages(&id);
        merge_user_query_into_tail(window.last_mut().unwrap(), "q2", ConversationManager::now_ms());
        mgr.replace_messages(&id, &window);
        mgr.append_messages(&id, &[assistant_msg("a2")]);

        assert_eq!(mgr.count_rounds(&id), 2);
        let exchanges = mgr.get_all_exchanges(&id);
        assert_eq!(exchanges.last().unwrap(), &("q2".to_string(), "a2".to_string()));
        let (_, _, count, last_q) = mgr.list_conversations().into_iter().next().unwrap();
        assert_eq!((count, last_q.as_str()), (2, "q2"));
        let md = mgr.collect_recent_conversations_md(std::time::UNIX_EPOCH);
        assert!(md.contains("q2"));
        assert!(!md.contains("SUMMARY TEXT"));
    }

    #[test]
    fn test_mini_compact_leaves_pre_compact_history_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = mgr.create_thread(ThreadMeta::default());
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("old_{}", r)));
        }
        let before_boundary = disk_messages(dir.path(), &id);
        full_compact(&mgr, &id, "S");
        for r in 0..3 {
            mgr.append_messages(&id, &big_tool_round(&format!("new_{}", r)));
        }

        let stats = mgr.compact_thread(&id).unwrap().expect("should compact");
        assert_eq!(stats.blocks, 1, "only the first live round is outside the protected window");
        let disk = disk_messages(dir.path(), &id);
        assert_eq!(&disk[..before_boundary.len()], &before_boundary[..]);
    }
}
