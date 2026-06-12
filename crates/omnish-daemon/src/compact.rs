//! Compaction of bulky tool_result blocks in stored chat history (issue #641).
//!
//! Pure functions only; snapshot + persistence live in ConversationManager.

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
}

/// True for a real user input message (has text content), as opposed to a
/// tool_result-only user message. Like ConversationManager::is_user_input, but
/// also accepts merged array-form user messages (text block present).
fn is_real_user_input(msg: &serde_json::Value) -> bool {
    if msg["role"].as_str() != Some("user") {
        return false;
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
    let mut stats = CompactStats::default();
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
    stats
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
}
