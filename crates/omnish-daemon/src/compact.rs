//! Compaction of stored chat history. Pure functions only; snapshot +
//! persistence live in ConversationManager.
//!
//! Two flavors:
//! - mini (`/compact mini`, issue #641): truncate bulky tool_result blocks
//!   and drop thinking blocks of old rounds. Non-destructive: the command only records a boundary
//!   (`ThreadMeta::mini_compact_upto`) and the truncation is re-applied to a
//!   copy of the history whenever an LLM request is built, see
//!   `apply_mini_compact`. `compact_messages` is the in-place form.
//! - full (`/compact`, issue #687): an LLM summary of the live context is
//!   appended as a user message tagged `_compact`. Nothing is deleted; the LLM
//!   context is rebuilt from the last such message onward, see
//!   `compact_boundary`. Also triggered automatically when the context nears
//!   the model's window (issue #691), see `estimate_context_tokens`.

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
    /// Number of old-round thinking blocks dropped (mini view only).
    pub thinking_blocks: usize,
    /// Total thinking text chars of those blocks.
    pub thinking_chars: usize,
}

impl CompactStats {
    /// True when the compaction changed nothing.
    pub fn is_noop(&self) -> bool {
        self.blocks == 0 && self.thinking_blocks == 0
    }
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
pub fn protect_boundary(msgs: &[serde_json::Value]) -> usize {
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

/// Truncate every bulky tool_result in `msgs` (no protected window).
fn truncate_tool_results(msgs: &mut [serde_json::Value], stats: &mut CompactStats) {
    for msg in msgs {
        if msg["role"].as_str() != Some("user") {
            continue;
        }
        let Some(blocks) = msg["content"].as_array_mut() else {
            continue;
        };
        for block in blocks {
            if block["type"].as_str() == Some("tool_result") {
                compact_tool_result(block, stats);
            }
        }
    }
}

/// Compact bulky tool_result blocks in-place, outside the protected window.
/// Returns stats; `blocks == 0` means nothing changed.
pub fn compact_messages(msgs: &mut [serde_json::Value]) -> CompactStats {
    let mut stats = CompactStats {
        total_before: total_chars(msgs),
        ..Default::default()
    };
    let boundary = protect_boundary(msgs);
    truncate_tool_results(&mut msgs[..boundary], &mut stats);
    stats.total_after = total_chars(msgs);
    stats
}

/// Drop thinking blocks from assistant messages. With tools enabled DeepSeek
/// asks for all previous reasoning to be passed back, but measured on its
/// Anthropic-compatible endpoint it accepts history without old-round
/// thinking, and an emptied block costs the same as a dropped one. Anthropic
/// documents omitting previous-turn thinking as allowed, whereas editing a
/// signed block is not. A message holding only thinking keeps one emptied
/// block so it does not become empty.
fn drop_thinking(msgs: &mut [serde_json::Value], stats: &mut CompactStats) {
    for msg in msgs {
        if msg["role"].as_str() != Some("assistant") {
            continue;
        }
        let Some(blocks) = msg["content"].as_array_mut() else {
            continue;
        };
        let is_thinking = |b: &serde_json::Value| b["type"].as_str() == Some("thinking");
        if !blocks.iter().any(is_thinking) {
            continue;
        }
        // Already reduced to the single emptied block: idempotent re-runs
        // must not count it again.
        if blocks.len() == 1 && blocks[0]["thinking"].as_str() == Some("") {
            continue;
        }
        for b in blocks.iter().filter(|b| is_thinking(b)) {
            stats.thinking_blocks += 1;
            stats.thinking_chars += b["thinking"].as_str().map_or(0, |t| t.chars().count());
        }
        if blocks.iter().all(is_thinking) {
            let mut kept = blocks[0].clone();
            kept["thinking"] = serde_json::Value::String(String::new());
            *blocks = vec![kept];
        } else {
            blocks.retain(|b| !is_thinking(b));
        }
    }
}

/// Apply a recorded mini compaction to a copy of the live window: truncate
/// bulky tool_results and drop thinking blocks in every message whose
/// absolute index (position in the whole thread) is below `upto`. `window_start` is the absolute index of
/// `window[0]`. Deterministic, so the same history always yields the same
/// request prefix and the prompt cache keeps hitting between turns.
pub fn apply_mini_compact(
    window: &mut [serde_json::Value],
    window_start: usize,
    upto: Option<usize>,
) -> CompactStats {
    let mut stats = CompactStats {
        total_before: total_chars(window),
        ..Default::default()
    };
    if let Some(upto) = upto {
        let end = upto.saturating_sub(window_start).min(window.len());
        truncate_tool_results(&mut window[..end], &mut stats);
        drop_thinking(&mut window[..end], &mut stats);
    }
    stats.total_after = total_chars(window);
    stats
}

// ---------------------------------------------------------------------------
// Full compaction (issue #687)
// ---------------------------------------------------------------------------

/// Message-level internal field marking a full-compaction summary message. Its
/// first content block is the summary; later text blocks are user queries the
/// chat path merged into it (it is a user-role tail until answered).
pub const COMPACT_FIELD: &str = "_compact";

/// Message-level internal field set (with `_compact`) on a summary produced
/// by auto compaction (issue #691) rather than the `/compact` command.
pub const COMPACT_AUTO_FIELD: &str = "_compact_auto";

/// Per-block caps when rendering the transcript sent to the summarizer.
const TRANSCRIPT_TEXT_MAX: usize = 8000;
const TRANSCRIPT_TOOL_INPUT_MAX: usize = 800;
const TRANSCRIPT_TOOL_RESULT_MAX: usize = 2000;

/// True when `msg` is a full-compaction summary message.
pub fn is_compact_message(msg: &serde_json::Value) -> bool {
    msg[COMPACT_FIELD].as_bool() == Some(true)
}

/// True when `msg` is a summary written by auto compaction.
pub fn is_auto_compact_message(msg: &serde_json::Value) -> bool {
    is_compact_message(msg) && msg[COMPACT_AUTO_FIELD].as_bool() == Some(true)
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

/// Append the compaction instruction to a copy of the live window the same
/// way the chat path appends a query: merged into a user-role tail (Anthropic
/// forbids two consecutive user messages), otherwise as a new user message.
/// Only the last message differs from the previous chat request, so the rest
/// of the prefix can be served from the provider's cache.
pub fn append_compact_instruction(msgs: &mut Vec<serde_json::Value>, instruction: &str, now_ms: u64) {
    match msgs.last_mut() {
        Some(tail) if crate::conversation_mgr::is_user_message(tail) => {
            crate::conversation_mgr::merge_user_query_into_tail(tail, instruction, now_ms);
        }
        _ => msgs.push(serde_json::json!({ "role": "user", "content": instruction })),
    }
}

/// Token estimate for an image block. Providers bill an image by its pixel
/// count (Anthropic: about w*h/750, capped near 1600 after their resize), not
/// by its base64 size, so images are counted at this flat rate.
pub const IMAGE_TOKEN_ESTIMATE: usize = 1600;

/// Rough token count of one stored message: UTF-8 bytes of its strings / 4
/// (about 1 token per English word piece, 0.75 per CJK char), images at
/// [`IMAGE_TOKEN_ESTIMATE`]. Internal `_` fields are skipped.
pub fn estimate_message_tokens(msg: &serde_json::Value) -> usize {
    fn bytes(v: &serde_json::Value, images: &mut usize) -> usize {
        match v {
            serde_json::Value::String(s) => s.len(),
            serde_json::Value::Array(a) => a.iter().map(|x| bytes(x, images)).sum(),
            serde_json::Value::Object(o) => {
                if o.get("type").and_then(|t| t.as_str()) == Some("image") {
                    *images += 1;
                    return 0;
                }
                o.iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(_, x)| bytes(x, images))
                    .sum()
            }
            _ => 0,
        }
    }
    let mut images = 0;
    let n = bytes(msg, &mut images);
    n.div_ceil(4) + images * IMAGE_TOKEN_ESTIMATE
}

/// Estimated context size in tokens of a chat request built from `msgs`, for
/// auto compaction (issue #691). `last_usage` is the total tokens (input,
/// cached and output) the provider reported for the most recent LLM call on
/// this context; it already covers everything up to and including the last
/// assistant message, so only the messages after it are estimated. Without
/// it the whole list is estimated.
pub fn estimate_context_tokens(last_usage: Option<u64>, msgs: &[&serde_json::Value]) -> u64 {
    let tail = match last_usage {
        Some(_) => {
            let last_assistant = msgs
                .iter()
                .rposition(|m| m["role"].as_str() == Some("assistant"));
            match last_assistant {
                Some(i) => &msgs[i + 1..],
                None => msgs,
            }
        }
        None => msgs,
    };
    let estimated: usize = tail.iter().map(|m| estimate_message_tokens(m)).sum();
    last_usage.unwrap_or(0) + estimated as u64
}

/// True when auto compaction should run: `used` tokens reach `percent`% of
/// `context_window`, and the messages themselves (`messages`, estimated
/// tokens of the live window) are at least half of that threshold. The rest
/// of `used` is the system prompt and tools, which a summary cannot shrink;
/// without the second check a context dominated by them would be compacted
/// before every LLM call. A zero percent or an unknown window disables it.
pub fn auto_compact_due(used: u64, messages: u64, context_window: Option<usize>, percent: u32) -> bool {
    match context_window {
        Some(cw) if cw > 0 && percent > 0 => {
            let threshold = cw as u64 * percent as u64;
            used * 100 >= threshold && messages * 200 >= threshold
        }
        _ => false,
    }
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
    fn auto_compact_marker() {
        let mut m = compact_msg("s");
        assert!(!is_auto_compact_message(&m));
        m[COMPACT_AUTO_FIELD] = serde_json::json!(true);
        assert!(is_auto_compact_message(&m));
        let mut plain = user_text("x");
        plain[COMPACT_AUTO_FIELD] = serde_json::json!(true);
        assert!(!is_auto_compact_message(&plain), "needs the _compact flag too");
    }

    #[test]
    fn estimate_message_tokens_counts_text_and_images() {
        // 400 ASCII bytes of text -> 100 tokens, plus the short "user"/"text" strings.
        let m = user_text(&"a".repeat(400));
        let t = estimate_message_tokens(&m);
        assert!((100..110).contains(&t), "got {t}");
        // Internal fields are ignored.
        let mut stamped = m.clone();
        stamped["_note"] = serde_json::json!("x".repeat(4000));
        assert_eq!(estimate_message_tokens(&stamped), t);
        // A large base64 payload counts as one image, not by its size.
        let img = serde_json::json!({"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "t", "content": [
                {"type": "text", "text": "Image"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                    "data": "A".repeat(1_000_000)}}
            ]}]});
        let t = estimate_message_tokens(&img);
        assert!((IMAGE_TOKEN_ESTIMATE..IMAGE_TOKEN_ESTIMATE + 20).contains(&t), "got {t}");
    }

    #[test]
    fn estimate_context_tokens_uses_usage_up_to_last_assistant() {
        let q1 = user_text(&"a".repeat(4000));
        let a1 = assistant_text("answer");
        let q2 = user_text(&"b".repeat(400));
        let msgs = [&q1, &a1, &q2];
        let tail = estimate_message_tokens(&q2) as u64;
        assert_eq!(estimate_context_tokens(Some(5000), &msgs), 5000 + tail);
        let all: u64 = msgs.iter().map(|m| estimate_message_tokens(m) as u64).sum();
        assert_eq!(estimate_context_tokens(None, &msgs), all);
        // No assistant yet: everything is new.
        assert_eq!(estimate_context_tokens(Some(10), &[&q2]), 10 + tail);
    }

    #[test]
    fn auto_compact_due_threshold() {
        assert!(!auto_compact_due(89_999, 80_000, Some(100_000), 90));
        assert!(auto_compact_due(90_000, 80_000, Some(100_000), 90));
        assert!(!auto_compact_due(1_000_000, 80_000, Some(100_000), 0), "0 disables");
        assert!(!auto_compact_due(1_000_000, 80_000, None, 90), "no window disables");
        assert!(!auto_compact_due(1_000_000, 80_000, Some(0), 90));
        // Messages must be at least half the threshold (45K of 90K).
        assert!(auto_compact_due(95_000, 45_000, Some(100_000), 90));
        assert!(!auto_compact_due(95_000, 44_999, Some(100_000), 90), "prompt-dominated context");
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

    #[test]
    fn instruction_appended_after_assistant_tail() {
        let mut msgs = vec![user_text("q"), assistant_text("a")];
        append_compact_instruction(&mut msgs, "SUMMARIZE", 1);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2], serde_json::json!({"role": "user", "content": "SUMMARIZE"}));
        assert_eq!(msgs[..2], [user_text("q"), assistant_text("a")]);
    }

    #[test]
    fn instruction_merged_into_user_tail() {
        // Interrupt snapshot: tool_result-only user tail.
        let mut msgs = vec![user_text("q"), assistant_tool_use("t1"), user_tool_result_str("t1", "out")];
        append_compact_instruction(&mut msgs, "SUMMARIZE", 1);
        assert_eq!(msgs.len(), 3, "no second consecutive user message");
        let blocks = msgs[2]["content"].as_array().unwrap();
        assert_eq!(blocks.last().unwrap()["text"], "SUMMARIZE");
        assert_eq!(blocks[0]["type"], "tool_result");
    }

    // --- non-destructive mini compaction ---

    #[test]
    fn apply_mini_compact_respects_absolute_boundary() {
        // 3 rounds of 4 messages; window starts at absolute index 100.
        let mut window = build_thread(3, true);
        let original = window.clone();

        // No boundary recorded: nothing changes.
        let stats = apply_mini_compact(&mut window, 100, None);
        assert_eq!(stats.blocks, 0);
        assert_eq!(window, original);

        // Boundary below the window: nothing changes either.
        assert_eq!(apply_mini_compact(&mut window, 100, Some(50)).blocks, 0);

        // Boundary covering round 0 only (absolute 100..104).
        let stats = apply_mini_compact(&mut window, 100, Some(104));
        assert_eq!(stats.blocks, 1);
        assert!(result_text(&window[2]).contains(COMPACT_MARKER));
        assert_eq!(window[4..], original[4..]);

        // Boundary past the end is clamped.
        let mut all = original.clone();
        assert_eq!(apply_mini_compact(&mut all, 100, Some(10_000)).blocks, 3);
    }

    #[test]
    fn apply_mini_compact_is_deterministic() {
        let mut a = build_thread(3, false);
        let mut b = a.clone();
        apply_mini_compact(&mut a, 0, Some(8));
        apply_mini_compact(&mut b, 0, Some(8));
        assert_eq!(a, b);
    }

    fn assistant_thinking_tool_use(id: &str, thought: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": [
            {"type": "thinking", "thinking": thought, "signature": "sig"},
            {"type": "tool_use", "id": id, "name": "bash", "input": {}}
        ]})
    }

    #[test]
    fn apply_mini_compact_drops_old_thinking_only() {
        let mut window = vec![
            user_text("q0"),
            assistant_thinking_tool_use("t0", "OLD THOUGHT"),
            user_tool_result_str("t0", "out"),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "ONLY", "signature": "s"}
            ]}),
            user_text("q1"),
            assistant_thinking_tool_use("t1", "NEW THOUGHT"),
        ];
        let stats = apply_mini_compact(&mut window, 0, Some(4));
        assert_eq!(stats.thinking_blocks, 2);
        assert_eq!(stats.thinking_chars, "OLD THOUGHT".len() + "ONLY".len());
        assert!(!stats.is_noop());

        // Dropped next to other blocks; the tool_use survives.
        assert_eq!(window[1]["content"].as_array().unwrap().len(), 1);
        assert_eq!(window[1]["content"][0]["type"], "tool_use");
        // A thinking-only message keeps one emptied block.
        assert_eq!(window[3]["content"], serde_json::json!([{"type": "thinking", "thinking": "", "signature": "s"}]));
        // Past the boundary: untouched.
        assert_eq!(window[5]["content"][0]["thinking"], "NEW THOUGHT");
    }

    #[test]
    fn apply_mini_compact_is_idempotent() {
        let mut window = vec![
            user_text("q0"),
            assistant_thinking_tool_use("t0", "T"),
            user_tool_result_str("t0", &many_lines(50)),
            serde_json::json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "ONLY", "signature": "s"}
            ]}),
            user_text("q1"),
        ];
        assert!(!apply_mini_compact(&mut window, 0, Some(4)).is_noop());
        let once = window.clone();
        assert!(apply_mini_compact(&mut window, 0, Some(4)).is_noop());
        assert_eq!(window, once);
    }

    #[test]
    fn apply_mini_compact_noop_without_boundary() {
        let mut window = vec![user_text("q"), assistant_thinking_tool_use("t", "T")];
        assert!(apply_mini_compact(&mut window, 0, None).is_noop());
        assert_eq!(window[1]["content"][0]["thinking"], "T");
    }
}
