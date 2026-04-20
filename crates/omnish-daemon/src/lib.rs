/// Remove `<thinking>...</thinking>` or `<think>...</think>` blocks,
/// keeping only the text after the closing tag.
pub fn strip_thinking_block(text: &str) -> String {
    let trimmed = text.trim_start();
    for (open, close) in [("<thinking>", "</thinking>"), ("<think>", "</think>")] {
        if let Some(rest) = trimmed.strip_prefix(open) {
            if let Some(end) = rest.find(close) {
                return rest[end + close.len()..].trim_start().to_string();
            }
            return String::new(); // unclosed tag
        }
    }
    text.to_string()
}

pub mod auto_update;
pub mod update_cache;
pub mod cleanup;
pub mod conversation_mgr;
pub mod daily_notes;
pub mod deploy;
pub mod eviction;
pub mod formatter_mgr;
pub mod hourly_summary;
pub mod plugin;
pub mod session_mgr;
pub mod task_mgr;
pub mod thread_summary;
pub mod tool_registry;
pub mod tools;
