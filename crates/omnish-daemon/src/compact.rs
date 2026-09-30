//! Compaction of stored chat history. Pure functions only; snapshot +
//! persistence live in ConversationManager.
//!
//! Two flavors:
//! - mini (`/compact mini`, issue #641): truncate bulky tool_result blocks in
//!   place, see `compact_messages`.
//! - full (`/compact`, issue #687): an LLM summary of the live context is
//!   appended as a user message tagged `_compact`. Nothing is deleted; the LLM
//!   context is rebuilt from the last such message onward, see
//!   `compact_boundary`.

/// Recent rounds (real user inputs counted from the tail) whose tool results
/// are never compacted.
pub const PROTECTED_ROUNDS: usize = 2;
/// Only compact tool_result texts longer than this many lines.
pub const MIN_LINES: usize = 30;
pub const HEAD_LINES: usize = 10;
pub const TAIL_LINES: usize = 10;
/// Marker embedded in compacted text. Also serves as the idempotency flag:
/// texts containing it are never compacted again.
pub const COMPACT_MARKER: &str = "[compacted:";

#[derive(Debug, Default, Clone, Copy)]
pub struct CompactStats {
    /// Number of tool_result blocks compacted.
    pub blocks: usize,
    /// Total chars of those blocks before compaction.
    pub chars_before: usize,
    /// Total chars of those blocks after compaction.
    pub chars_after: usize,
    /// Total chars of the whole message list before compaction (serialized,
    /// covering text, thinking, tool_use inputs and tool_results alike). A
    /// proxy for how much the LLM context shrank, not just the touched blocks.
    pub total_before: usize,
    /// Total chars of the whole message list after compaction.
    pub total_after: usize,
}

/// Total serialized char count across all messages. Consistent before/after
/// measure of the whole context size.
fn total_chars(msgs: &[serde_json::Value]) -> usize {
    msgs.iter()
        .map(|m| serde_json::to_string(m).map(|s| s.chars().count()).unwrap_or(0))
        .sum()
}

/// True for a real user input message (has text content), as opposed to a
/// tool_result-only user message. Like ConversationManager::is_user_input, but
/// also accepts merged array-form user messages (text block present).
fn is_real_user_input(msg: &serde_json::Value) -> bool {
    if msg["role"].as_str() != Some("user") {
        return false;
    }
    if is_compact_message(msg) {
        return compact_has_query(msg);
    }
    match &msg["content"] {
        serde_json::Value::String(_) => true,
        serde_json::Value::Array(arr) => {
            arr.iter().any(|b| b["type"].as_str() == Some("text"))
        }
        _ => false,
    }
}

/// Index of the first message in the protected window: everything from the
/// PROTECTED_ROUNDS-th real user input (counted from the tail) onward.
/// Returns 0 when the thread has fewer rounds than the window.
fn protect_boundary(msgs: &[serde_json::Value]) -> usize {
    let mut rounds = 0;
    for (i, m) in msgs.iter().enumerate().rev() {
        if is_real_user_input(m) {
            rounds += 1;
            if rounds == PROTECTED_ROUNDS {
                return i;
            }
        }
    }
    0
}

/// Truncate `text` to head + marker + tail. None when below threshold or
/// already compacted.
fn compact_text(text: &str) -> Option<String> {
    if text.contains(COMPACT_MARKER) {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MIN_LINES {
        return None;
    }
    let head = lines[..HEAD_LINES].join("\n");
    let tail = lines[lines.len() - TAIL_LINES..].join("\n");
    let omitted_lines = lines.len() - HEAD_LINES - TAIL_LINES;
    // Approximate: includes newlines belonging to the omitted region.
    let omitted_chars = text.chars().count().saturating_sub(head.chars().count() + tail.chars().count());
    Some(format!(
        "{}\n... {} omitted {} lines, {} chars] ...\n{}",
        head, COMPACT_MARKER, omitted_lines, omitted_chars, tail
    ))
}

fn compact_tool_result(block: &mut serde_json::Value, stats: &mut CompactStats) {
    let mut chars_before = 0;
    let mut chars_after = 0;
    let mut changed = false;
    match &mut block["content"] {
        serde_json::Value::String(s) => {
            if let Some(c) = compact_text(s) {
                chars_before += s.chars().count();
                chars_after += c.chars().count();
                *s = c;
                changed = true;
            }
        }
        serde_json::Value::Array(arr) => {
            for inner in arr.iter_mut() {
                if inner["type"].as_str() != Some("text") {
                    continue;
                }
                if let serde_json::Value::String(s) = &mut inner["text"] {
                    if let Some(c) = compact_text(s) {
                        chars_before += s.chars().count();
                        chars_after += c.chars().count();
                        *s = c;
                        changed = true;
                    }
                }
            }
        }
        _ => {}
    }
    if changed {
        stats.blocks += 1;
        stats.chars_before += chars_before;
        stats.chars_after += chars_after;
    }
}

/// Compact bulky tool_result blocks in-place. Returns stats; `blocks == 0`
/// means nothing changed.
pub fn compact_messages(msgs: &mut [serde_json::Value]) -> CompactStats {
    let mut stats = CompactStats {
        total_before: total_chars(msgs),
        ..Default::default()
    };
    let boundary = protect_boundary(msgs);
    for msg in &mut msgs[..boundary] {
        if msg["role"].as_str() != Some("user") {
            continue;
        }
        let Some(blocks) = msg["content"].as_array_mut() else {
            continue;
        };
        for block in blocks {
            if block["type"].as_str() == Some("tool_result") {
                compact_tool_result(block, &mut stats);
            }
        }
    }
    stats.total_after = total_chars(msgs);
    stats
}

// ---------------------------------------------------------------------------
// Full compaction (issue #687)
// ---------------------------------------------------------------------------

/// Message-level internal field marking a full-compaction summary message. Its
/// first content block is the summary; later text blocks are user queries the
/// chat path merged into it (it is a user-role tail until answered).
pub const COMPACT_FIELD: &str = "_compact";

/// Per-block caps when rendering the transcript sent to the summarizer.
const TRANSCRIPT_TEXT_MAX: usize = 8000;
const TRANSCRIPT_TOOL_INPUT_MAX: usize = 800;
const TRANSCRIPT_TOOL_RESULT_MAX: usize = 2000;

/// True when `msg` is a full-compaction summary message.
pub fn is_compact_message(msg: &serde_json::Value) -> bool {
    msg[COMPACT_FIELD].as_bool() == Some(true)
}

/// True when content block `idx` of `msg` is the compaction summary itself
/// (as opposed to a user query merged into the message afterwards). Display
/// and summary paths use this to hide the summary text.
pub fn is_compact_summary_block(msg: &serde_json::Value, idx: usize) -> bool {
    idx == 0 && is_compact_message(msg)
}

/// True when a user query has been merged into a compact message, i.e. it has
/// a text block beyond the summary.
pub fn compact_has_query(msg: &serde_json::Value) -> bool {
    msg["content"].as_array().is_some_and(|arr| {
        arr.iter().skip(1).any(|b| b["type"].as_str() == Some("text"))
    })
}

/// Index where the live LLM context starts: the last compact message, or 0
/// when the thread was never fully compacted.
pub fn compact_boundary(msgs: &[serde_json::Value]) -> usize {
    msgs.iter().rposition(is_compact_message).unwrap_or(0)
}

/// False when the live window holds nothing a new summary could add: empty,
/// or only the previous summary with no follow-up.
pub fn has_content_to_compact(window: &[serde_json::Value]) -> bool {
    match window {
        [] => false,
        [only] => !is_compact_message(only) || compact_has_query(only),
        _ => true,
    }
}

/// Build the summary message appended by a full compaction. `text` is the
/// complete block text (header + LLM summary).
pub fn make_compact_message(text: &str, now_ms: u64) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": text, "_ts": now_ms }],
        COMPACT_FIELD: true,
        "_ts": now_ms,
    })
}

/// Serialized char count of a message list (same measure as mini stats).
pub fn messages_chars(msgs: &[serde_json::Value]) -> usize {
    total_chars(msgs)
}

/// Keep the head and tail of `text` when it exceeds `max` chars.
fn clip(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(n - half).collect();
    format!("{}\n... [{} chars omitted] ...\n{}", head, n - 2 * half, tail)
}

fn tool_result_text(block: &serde_json::Value) -> String {
    match &block["content"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Render one message as transcript entries.
fn transcript_entries(msg: &serde_json::Value, out: &mut Vec<String>) {
    let is_user = msg["role"].as_str() == Some("user");
    let speaker = if is_user { "User" } else { "Assistant" };
    let blocks = match &msg["content"] {
        serde_json::Value::String(s) => {
            out.push(format!("{}: {}", speaker, clip(s, TRANSCRIPT_TEXT_MAX)));
            return;
        }
        serde_json::Value::Array(arr) => arr,
        _ => return,
    };
    for (idx, b) in blocks.iter().enumerate() {
        match b["type"].as_str() {
            Some("text") => {
                let text = b["text"].as_str().unwrap_or("");
                if text.trim().is_empty() {
                    continue;
                }
                if is_compact_summary_block(msg, idx) {
                    // Carried forward verbatim so repeated compactions keep it.
                    out.push(format!("[Summary of the conversation before this point]\n{}", text));
                } else {
                    out.push(format!("{}: {}", speaker, clip(text, TRANSCRIPT_TEXT_MAX)));
                }
            }
            Some("tool_use") => {
                let name = b["name"].as_str().unwrap_or("?");
                let input = serde_json::to_string(&b["input"]).unwrap_or_default();
                out.push(format!("Tool call ({}): {}", name, clip(&input, TRANSCRIPT_TOOL_INPUT_MAX)));
            }
            Some("tool_result") => {
                let label = if b["is_error"].as_bool() == Some(true) { "Tool error" } else { "Tool result" };
                out.push(format!("{}: {}", label, clip(&tool_result_text(b), TRANSCRIPT_TOOL_RESULT_MAX)));
            }
            // thinking / redacted_thinking: internal reasoning, not needed.
            _ => {}
        }
    }
}

/// Render the live window as a plain-text transcript for the summarizer. A
/// text transcript (rather than raw messages) sidesteps protocol constraints
/// of the raw form: role alternation, tool definitions for tool_use blocks
/// and thinking-block round-tripping.
///
/// When the rendered text exceeds `budget` chars the oldest entries are
/// dropped, except a leading previous summary which is always kept.
pub fn build_transcript(window: &[serde_json::Value], budget: usize) -> String {
    let mut entries = Vec::new();
    for msg in window {
        transcript_entries(msg, &mut entries);
    }
    let pinned = usize::from(window.first().is_some_and(is_compact_message) && !entries.is_empty());
    let sep = 2; // "\n\n" between entries
    let mut total: usize = entries.iter().map(|e| e.chars().count() + sep).sum();
    let mut drop_from = pinned;
    let mut dropped = 0;
    while total > budget && drop_from < entries.len().saturating_sub(1) {
        total -= entries[drop_from].chars().count() + sep;
        drop_from += 1;
        dropped += 1;
    }
    let mut kept: Vec<String> = entries[..pinned].to_vec();
    if dropped > 0 {
        kept.push(format!("[... {} earlier entries omitted ...]", dropped));
    }
    kept.extend_from_slice(&entries[drop_from..]);
    kept.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn many_lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n")
    }

    fn user_text(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    fn assistant_tool_use(id: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": "bash", "input": {}}
        ]})
    }

    /// tool_result whose content is a plain string.
    fn user_tool_result_str(id: &str, content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": content}
        ]})
    }

    /// tool_result whose content is an array of text blocks.
    fn user_tool_result_arr(id: &str, content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": [
                {"type": "text", "text": content}
            ]}
        ]})
    }

    fn assistant_text(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": [
            {"type": "text", "text": text}
        ]})
    }

    /// Build a thread with `rounds` rounds; each round is:
    /// user text, assistant tool_use, user tool_result (big), assistant text.
    fn build_thread(rounds: usize, string_content: bool) -> Vec<serde_json::Value> {
        let mut msgs = Vec::new();
        for r in 0..rounds {
            let id = format!("tu_{}", r);
            msgs.push(user_text(&format!("question {}", r)));
            msgs.push(assistant_tool_use(&id));
            let big = many_lines(50);
            msgs.push(if string_content {
                user_tool_result_str(&id, &big)
            } else {
                user_tool_result_arr(&id, &big)
            });
            msgs.push(assistant_text(&format!("answer {}", r)));
        }
        msgs
    }

    fn result_text(msg: &serde_json::Value) -> String {
        let block = &msg["content"][0];
        match &block["content"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(arr) => arr[0]["text"].as_str().unwrap().to_string(),
            _ => panic!("unexpected content"),
        }
    }

    #[test]
    fn compacts_string_content_outside_protected_window() {
        // 3 rounds: round 0 is outside the protected window (last 2 rounds).
        let mut msgs = build_thread(3, true);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 1);
        assert!(stats.chars_before > stats.chars_after);
        let compacted = result_text(&msgs[2]);
        assert!(compacted.contains(COMPACT_MARKER));
        assert!(compacted.starts_with("line 1\n"));
        assert!(compacted.ends_with("line 50"));
        // Protected rounds untouched
        assert!(!result_text(&msgs[6]).contains(COMPACT_MARKER));
        assert!(!result_text(&msgs[10]).contains(COMPACT_MARKER));
    }

    #[test]
    fn compacts_array_content() {
        let mut msgs = build_thread(3, false);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 1);
        assert!(result_text(&msgs[2]).contains(COMPACT_MARKER));
    }

    #[test]
    fn reports_whole_context_size() {
        let mut msgs = build_thread(3, true);
        let stats = compact_messages(&mut msgs);
        // Whole-context totals cover all messages, so they exceed the
        // single touched block's char counts.
        assert!(stats.total_before > stats.chars_before);
        assert!(stats.total_after < stats.total_before);
        assert!(stats.total_after > stats.chars_after);
    }

    #[test]
    fn totals_unchanged_when_nothing_compacts() {
        // Two rounds: fully protected, nothing compacts; totals stay equal.
        let mut msgs = build_thread(2, true);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 0);
        assert_eq!(stats.total_before, stats.total_after);
    }

    #[test]
    fn protects_recent_rounds_when_thread_is_short() {
        // Only 2 rounds total: everything is inside the protected window.
        let mut msgs = build_thread(2, true);
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 0);
        assert!(!result_text(&msgs[2]).contains(COMPACT_MARKER));
    }

    #[test]
    fn line_threshold_boundary() {
        // 30 lines: not compacted. 31 lines: compacted.
        let id30 = "tu_30";
        let id31 = "tu_31";
        let mut msgs = vec![
            user_text("q0"),
            assistant_tool_use(id30),
            user_tool_result_str(id30, &many_lines(30)),
            assistant_text("a0"),
            user_text("q1"),
            assistant_tool_use(id31),
            user_tool_result_str(id31, &many_lines(31)),
            assistant_text("a1"),
            // Two more rounds so both results above are unprotected
            user_text("q2"),
            assistant_text("a2"),
            user_text("q3"),
            assistant_text("a3"),
        ];
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 1);
        assert!(!result_text(&msgs[2]).contains(COMPACT_MARKER));
        assert!(result_text(&msgs[6]).contains(COMPACT_MARKER));
    }

    #[test]
    fn idempotent_on_second_run() {
        let mut msgs = build_thread(3, true);
        let first = compact_messages(&mut msgs);
        assert_eq!(first.blocks, 1);
        let snapshot = msgs.clone();
        let second = compact_messages(&mut msgs);
        assert_eq!(second.blocks, 0);
        assert_eq!(msgs, snapshot);
    }

    #[test]
    fn ignores_non_tool_result_blocks_and_string_user_messages() {
        let big = many_lines(50);
        let mut msgs = vec![
            // Plain string user message with many lines: not a tool_result
            user_text(&big),
            assistant_text("a0"),
            user_text("q1"),
            assistant_text("a1"),
            user_text("q2"),
            assistant_text("a2"),
        ];
        let stats = compact_messages(&mut msgs);
        assert_eq!(stats.blocks, 0);
        assert_eq!(msgs[0]["content"].as_str().unwrap(), big);
    }

    // --- full compaction (issue #687) ---

    fn compact_msg(summary: &str) -> serde_json::Value {
        make_compact_message(summary, 1)
    }

    /// Simulate the chat path merging a user query into a compact tail.
    fn with_merged_query(mut msg: serde_json::Value, query: &str) -> serde_json::Value {
        msg["content"].as_array_mut().unwrap().push(serde_json::json!({"type": "text", "text": query}));
        msg
    }

    #[test]
    fn boundary_is_zero_without_compact_message() {
        let msgs = build_thread(2, true);
        assert_eq!(compact_boundary(&msgs), 0);
        assert_eq!(compact_boundary(&[]), 0);
    }

    #[test]
    fn boundary_points_at_last_compact_message() {
        let mut msgs = build_thread(1, true);
        msgs.push(compact_msg("s1"));
        let first = msgs.len() - 1;
        assert_eq!(compact_boundary(&msgs), first);

        msgs.push(assistant_text("after s1"));
        msgs.push(user_text("q"));
        msgs.push(compact_msg("s2"));
        assert_eq!(compact_boundary(&msgs), msgs.len() - 1);
    }

    #[test]
    fn compact_message_shape() {
        let m = compact_msg("hello");
        assert!(is_compact_message(&m));
        assert_eq!(m["role"], "user");
        assert_eq!(m["content"][0]["text"], "hello");
        assert!(is_compact_summary_block(&m, 0));
        assert!(!is_compact_summary_block(&m, 1));
        assert!(!is_compact_message(&user_text("x")));
        assert!(!is_compact_summary_block(&user_text("x"), 0));
    }

    #[test]
    fn merged_query_detection() {
        let m = compact_msg("s");
        assert!(!compact_has_query(&m));
        assert!(!is_real_user_input(&m), "bare summary is not a round");
        let merged = with_merged_query(m, "next question");
        assert!(compact_has_query(&merged));
        assert!(is_real_user_input(&merged));
    }

    #[test]
    fn nothing_to_compact_detection() {
        assert!(!has_content_to_compact(&[]));
        assert!(!has_content_to_compact(&[compact_msg("s")]));
        assert!(has_content_to_compact(&[with_merged_query(compact_msg("s"), "q")]));
        assert!(has_content_to_compact(&[compact_msg("s"), assistant_text("a")]));
        assert!(has_content_to_compact(&[user_text("q")]));
    }

    #[test]
    fn transcript_renders_roles_tools_and_summary() {
        let mut window = vec![with_merged_query(compact_msg("OLD SUMMARY"), "q1")];
        window.push(serde_json::json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": "SECRET REASONING"},
            {"type": "text", "text": "let me look"},
            {"type": "tool_use", "id": "t1", "name": "bash", "input": {"command": "ls"}}
        ]}));
        window.push(user_tool_result_str("t1", "file_a\nfile_b"));
        window.push(assistant_text("done"));
        let t = build_transcript(&window, 100_000);
        assert!(t.starts_with("[Summary of the conversation before this point]\nOLD SUMMARY"));
        assert!(t.contains("User: q1"));
        assert!(t.contains("Assistant: let me look"));
        assert!(t.contains("Tool call (bash): {\"command\":\"ls\"}"));
        assert!(t.contains("Tool result: file_a\nfile_b"));
        assert!(t.contains("Assistant: done"));
        assert!(!t.contains("SECRET REASONING"));
    }

    #[test]
    fn transcript_clips_large_tool_results() {
        let big = "x".repeat(10_000);
        let window = vec![user_text("q"), assistant_tool_use("t1"), user_tool_result_str("t1", &big)];
        let t = build_transcript(&window, 1_000_000);
        assert!(t.contains("chars omitted"));
        assert!(t.chars().count() < 3_000);
    }

    #[test]
    fn transcript_over_budget_drops_oldest_but_keeps_summary() {
        let mut window = vec![with_merged_query(compact_msg("KEEP ME"), "first question")];
        for i in 0..50 {
            window.push(assistant_text(&format!("answer {} {}", i, "y".repeat(200))));
            window.push(user_text(&format!("question {}", i)));
        }
        let t = build_transcript(&window, 2_000);
        assert!(t.starts_with("[Summary of the conversation before this point]\nKEEP ME"));
        assert!(t.contains("earlier entries omitted"));
        assert!(t.contains("question 49"), "newest entries survive");
        assert!(!t.contains("first question"), "oldest non-summary entries dropped");
    }
}
