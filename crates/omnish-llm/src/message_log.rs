use std::fs;
use std::path::PathBuf;

use crate::backend::UseCase;

const MAX_LOG_FILES: usize = 60;

fn log_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".omnish/logs/messages"))
}

/// Roll the message log directory down to MAX_LOG_FILES, deleting the oldest.
/// Counts both `.req.json` and `.resp.json` files toward the cap so a busy
/// session doesn't exceed the bound.
fn cleanup_log_dir(dir: &PathBuf) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    if files.len() > MAX_LOG_FILES {
        files.sort();
        for old in &files[..files.len() - MAX_LOG_FILES] {
            let _ = fs::remove_file(old);
        }
    }
}

fn write_pretty_json(path: &PathBuf, body: &serde_json::Value) {
    if let Ok(json) = serde_json::to_string_pretty(body) {
        let _ = fs::write(path, json);
    }
}

/// Save the full LLM request body to ~/.omnish/logs/messages/{timestamp}.req.json.
/// Returns the timestamp prefix so the matching response log can pair with it.
/// Only logs when use_case is Chat. Returns None when logging is skipped.
pub fn log_request(body: &serde_json::Value, use_case: UseCase) -> Option<String> {
    if use_case != UseCase::Chat {
        return None;
    }
    let dir = log_dir()?;
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S%.3f").to_string();
    let path = dir.join(format!("{}.req.json", timestamp));
    write_pretty_json(&path, body);
    cleanup_log_dir(&dir);
    Some(timestamp)
}

/// Save the LLM response body alongside its request log entry. The `tag` is
/// the timestamp returned by [`log_request`], so the pair shares a prefix on
/// disk: `{tag}.req.json` + `{tag}.resp.json`. Pass `None` to skip (e.g. when
/// the matching request wasn't logged).
pub fn log_response(tag: Option<&str>, body: &serde_json::Value) {
    let Some(tag) = tag else { return };
    let Some(dir) = log_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{}.resp.json", tag));
    write_pretty_json(&path, body);
    cleanup_log_dir(&dir);
}
