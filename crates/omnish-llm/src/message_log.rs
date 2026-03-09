use std::fs;
use std::path::PathBuf;

const MAX_LOG_FILES: usize = 30;

fn log_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".omnish/logs/messages"))
}

/// Save the full LLM request body to ~/.omnish/logs/messages/{timestamp}.json.
/// Keeps only the most recent MAX_LOG_FILES files (rolling cleanup).
pub fn log_request(body: &serde_json::Value) {
    let Some(dir) = log_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S%.3f");
    let path = dir.join(format!("{}.json", timestamp));

    if let Ok(json) = serde_json::to_string_pretty(body) {
        let _ = fs::write(&path, json);
    }

    // Rolling cleanup: keep only the last MAX_LOG_FILES
    if let Ok(entries) = fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext == "json"))
            .collect();

        if files.len() > MAX_LOG_FILES {
            files.sort();
            for old in &files[..files.len() - MAX_LOG_FILES] {
                let _ = fs::remove_file(old);
            }
        }
    }
}
