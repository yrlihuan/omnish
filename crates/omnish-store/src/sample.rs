use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};

/// A pending sample buffered in the daemon session, waiting for the next command.
#[derive(Debug, Clone)]
pub struct PendingSample {
    pub session_id: String,
    pub context: String,
    pub prompt: String,
    pub suggestions: Vec<String>,
    pub input: String,
    pub cwd: Option<String>,
    pub latency_ms: u64,
    pub accepted: bool,
}

/// A completed sample record written to JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSample {
    pub recorded_at: String,
    pub session_id: String,
    pub context: String,
    pub prompt: String,
    pub suggestions: Vec<String>,
    pub input: String,
    pub accepted: bool,
    pub next_command: Option<String>,
    pub similarity: Option<f64>,
    pub cwd: Option<String>,
    pub latency_ms: u64,
}

/// Levenshtein edit distance between two strings.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Similarity ratio (0.0 = completely different, 1.0 = identical).
pub fn similarity(a: &str, b: &str) -> f64 {
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(a, b) as f64 / max_len as f64)
}

/// Spawn a writer thread that writes CompletionSample records to daily-rotated JSONL files.
pub fn spawn_sample_writer(samples_dir: PathBuf) -> mpsc::Sender<CompletionSample> {
    let (tx, rx): (mpsc::Sender<CompletionSample>, mpsc::Receiver<CompletionSample>) =
        mpsc::channel();

    thread::spawn(move || {
        std::fs::create_dir_all(&samples_dir).ok();
        let mut current_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut writer: Option<Box<dyn std::io::Write + Send>> = None;

        loop {
            match rx.try_recv() {
                Ok(sample) => {
                    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                    if today != current_date {
                        current_date = today;
                        writer = None;
                    }
                    if writer.is_none() {
                        let path = samples_dir.join(format!("{}.jsonl", current_date));
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            Ok(file) => writer = Some(Box::new(file)),
                            Err(e) => {
                                tracing::error!("Failed to open samples file: {}", e);
                                continue;
                            }
                        }
                    }
                    if let Some(ref mut w) = writer {
                        if let Ok(json) = serde_json::to_string(&sample) {
                            let _ = writeln!(w, "{}", json);
                            let _ = w.flush();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
    });

    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("git status", "git status"), 0);
    }

    #[test]
    fn test_levenshtein_one_edit() {
        assert_eq!(levenshtein("git status", "git statu"), 1);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_similarity_identical() {
        let s = similarity("git status", "git status");
        assert!((s - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_similarity_near_miss() {
        let s = similarity("git status --short", "git status -s");
        assert!(s > 0.3 && s < 1.0);
    }

    #[test]
    fn test_similarity_completely_different() {
        let s = similarity("ls -la", "docker compose up");
        assert!(s < 0.3);
    }

    #[test]
    fn test_similarity_empty() {
        assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("abc", "")).abs() < f64::EPSILON);
    }

    #[test]
    fn test_sample_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        let tx = spawn_sample_writer(dir.path().to_path_buf());

        let sample = CompletionSample {
            recorded_at: "2026-03-03T12:00:00".to_string(),
            session_id: "test".to_string(),
            context: "$ ls\nfile.txt".to_string(),
            prompt: "You are a completion engine...".to_string(),
            suggestions: vec!["git status".to_string()],
            input: "git st".to_string(),
            accepted: false,
            next_command: Some("git status -s".to_string()),
            similarity: Some(0.61),
            cwd: Some("/tmp".to_string()),
            latency_ms: 150,
        };
        tx.send(sample).unwrap();
        thread::sleep(std::time::Duration::from_millis(100));
        drop(tx);
        thread::sleep(std::time::Duration::from_millis(100));

        let entries = std::fs::read_dir(dir.path()).unwrap();
        let jsonl_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "jsonl").unwrap_or(false))
            .collect();
        assert_eq!(jsonl_files.len(), 1);

        let content = std::fs::read_to_string(jsonl_files[0].path()).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: CompletionSample = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.session_id, "test");
        assert_eq!(parsed.next_command.as_deref(), Some("git status -s"));
    }
}
