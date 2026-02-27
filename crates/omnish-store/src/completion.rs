use serde::Deserialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct CompletionRecord {
    /// Session ID
    pub session_id: String,
    /// Sequence ID of the completion request
    pub sequence_id: u64,
    /// User input at the time of request
    pub prompt: String,
    /// The suggested completion text
    pub completion: String,
    /// Whether the user accepted the completion (Tab key)
    pub accepted: bool,
    /// Time from request to response (milliseconds)
    pub latency_ms: u64,
    /// Time from response to accept/ignore (milliseconds)
    pub dwell_time_ms: Option<u64>,
    /// Current working directory at the time of request
    pub cwd: Option<String>,
    /// Timestamp when this record was created (epoch ms)
    pub recorded_at: u64,
}

impl CompletionRecord {
    /// Convert to CSV row
    pub fn to_csv_row(&self) -> String {
        let dwell = self.dwell_time_ms.map(|d| d.to_string()).unwrap_or_default();
        let cwd = self.cwd.as_deref().unwrap_or("");
        // Escape fields that might contain commas or newlines
        let escape = |s: &str| {
            if s.contains(',') || s.contains('\n') || s.contains('"') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        format!(
            "{},{},{},{},{},{},{},{},{}\n",
            self.recorded_at,
            self.session_id,
            self.sequence_id,
            escape(&self.prompt),
            escape(&self.completion),
            self.accepted,
            self.latency_ms,
            dwell,
            escape(cwd)
        )
    }

    /// CSV header
    pub fn csv_header() -> &'static str {
        "recorded_at,session_id,sequence_id,prompt,completion,accepted,latency_ms,dwell_time_ms,cwd\n"
    }
}

/// Spawn a writer thread that handles completion records asynchronously
pub fn spawn_writer_thread(completions_dir: PathBuf) -> mpsc::Sender<CompletionRecord> {
    let (tx, rx): (mpsc::Sender<CompletionRecord>, mpsc::Receiver<CompletionRecord>) = mpsc::channel();

    thread::spawn(move || {
        // Ensure directory exists
        std::fs::create_dir_all(&completions_dir).ok();

        // Track current date to handle daily rotation
        let mut current_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let mut writer: Option<Box<dyn std::io::Write + Send>> = None;

        loop {
            // Try to receive without blocking
            match rx.try_recv() {
                Ok(record) => {
                    // Check if we need to rotate to a new day
                    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                    if today != current_date {
                        current_date = today;
                        writer = None;
                    }

                    // Ensure we have a writer for today
                    if writer.is_none() {
                        let path = completions_dir.join(format!("{}.csv", current_date));
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            Ok(mut file) => {
                                // If file is empty, write header
                                if let Ok(metadata) = file.metadata() {
                                    if metadata.len() == 0 {
                                        let _ = file.write_all(CompletionRecord::csv_header().as_bytes());
                                    }
                                }
                                writer = Some(Box::new(file));
                            }
                            Err(e) => {
                                tracing::error!("Failed to open completions file: {}", e);
                                continue;
                            }
                        }
                    }

                    // Write the record
                    if let Some(ref mut w) = writer {
                        if let Err(e) = w.write_all(record.to_csv_row().as_bytes()) {
                            tracing::error!("Failed to write completion record: {}", e);
                        }
                        if let Err(e) = w.flush() {
                            tracing::error!("Failed to flush completion record: {}", e);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // No message, sleep briefly to avoid busy loop
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Channel closed, exit thread
                    break;
                }
            }
        }
    });

    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_csv_format() {
        let record = CompletionRecord {
            session_id: "test".to_string(),
            sequence_id: 1,
            prompt: "git".to_string(),
            completion: "status".to_string(),
            accepted: true,
            latency_ms: 100,
            dwell_time_ms: Some(50),
            cwd: Some("/home/user/project".to_string()),
            recorded_at: 1709000000000,
        };

        let row = record.to_csv_row();
        assert!(row.contains("1709000000000"));
        assert!(row.contains("test"));
        assert!(row.contains("1"));
        assert!(row.contains("git"));
        assert!(row.contains("status"));
        assert!(row.contains("true"));
        assert!(row.contains("100"));
        assert!(row.contains("50"));
        assert!(row.contains("/home/user/project"));
    }

    #[test]
    fn test_csv_with_comma_in_field() {
        let record = CompletionRecord {
            session_id: "test".to_string(),
            sequence_id: 1,
            prompt: "echo hello, world".to_string(),
            completion: "test".to_string(),
            accepted: false,
            latency_ms: 100,
            dwell_time_ms: None,
            cwd: None,
            recorded_at: 1709000000000,
        };

        let row = record.to_csv_row();
        assert!(row.contains("\"echo hello, world\""));
    }

    #[test]
    fn test_csv_header() {
        let header = CompletionRecord::csv_header();
        assert!(header.starts_with("recorded_at"));
        assert!(header.contains("session_id"));
        assert!(header.contains("accepted"));
    }

    #[test]
    fn test_writer_thread() {
        let dir = TempDir::new().unwrap();
        let tx = spawn_writer_thread(dir.path().to_path_buf());

        // Send a few records
        for i in 0..5 {
            let record = CompletionRecord {
                session_id: "test".to_string(),
                sequence_id: i,
                prompt: format!("prompt{}", i),
                completion: format!("completion{}", i),
                accepted: i % 2 == 0,
                latency_ms: 100 + i,
                dwell_time_ms: Some(50),
                cwd: Some("/tmp".to_string()),
                recorded_at: 1709000000000 + i,
            };
            tx.send(record).unwrap();
        }

        // Give time for writer to process
        thread::sleep(std::time::Duration::from_millis(100));

        // Drop sender to signal thread to exit
        drop(tx);
        thread::sleep(std::time::Duration::from_millis(100));

        // Check that file was created with content
        let entries = fs::read_dir(dir.path()).unwrap();
        let csv_files: Vec<_> = entries.filter_map(|e| e.ok()).filter(|e| {
            e.path().extension().map(|ext| ext == "csv").unwrap_or(false)
        }).collect();

        assert_eq!(csv_files.len(), 1);

        let content = fs::read_to_string(csv_files[0].path()).unwrap();
        // Should have header + 5 records
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 6); // header + 5 records
    }
}
