use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct SessionUpdateRecord {
    /// Session ID
    pub session_id: String,
    /// Timestamp when this record was created (epoch ms)
    pub timestamp_ms: u64,
    /// Hostname from probe
    pub host: Option<String>,
    /// Shell current working directory from probe
    pub shell_cwd: Option<String>,
    /// Child process info from probe
    pub child_process: Option<String>,
    /// Extra fields (attrs other than host, shell_cwd, child_process for future extension)
    #[serde(default)]
    pub extra: HashMap<String, Value>,
}

impl SessionUpdateRecord {
    /// Convert epoch milliseconds to readable timestamp
    fn format_timestamp(ts_ms: u64) -> String {
        // Use chrono::DateTime::from_timestamp_millis to convert
        if let Some(datetime) = chrono::DateTime::from_timestamp_millis(ts_ms as i64) {
            datetime.format("%Y-%m-%d %H:%M:%S").to_string()
        } else {
            ts_ms.to_string()
        }
    }

    /// Create a new SessionUpdateRecord, extracting known fields from attrs
    pub fn new(session_id: String, timestamp_ms: u64, attrs: HashMap<String, String>) -> Self {
        // Extract known fields
        let host = attrs.get("host").cloned();
        let shell_cwd = attrs.get("shell_cwd").cloned();
        let child_process = attrs.get("child_process").cloned();

        // Extract fields other than host, shell_cwd, child_process into extra
        let mut extra = HashMap::new();
        let known_fields = ["host", "shell_cwd", "child_process"];
        for (k, v) in attrs.iter() {
            if !known_fields.contains(&k.as_str()) {
                extra.insert(k.clone(), Value::String(v.clone()));
            }
        }

        Self {
            session_id,
            timestamp_ms,
            host,
            shell_cwd,
            child_process,
            extra,
        }
    }

    /// Convert to CSV row
    pub fn to_csv_row(&self) -> String {
        // Serialize extra as JSON string
        let extra_json = serde_json::to_string(&self.extra).unwrap_or_default();
        // Escape fields that might contain commas or newlines
        let escape = |s: &str| {
            if s.contains(',') || s.contains('\n') || s.contains('"') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        // Optional fields as empty string if None
        let host = self.host.as_deref().unwrap_or("");
        let shell_cwd = self.shell_cwd.as_deref().unwrap_or("");
        let child_process = self.child_process.as_deref().unwrap_or("");
        format!(
            "{},{},{},{},{},{}\n",
            Self::format_timestamp(self.timestamp_ms),
            escape(&self.session_id),
            escape(host),
            escape(shell_cwd),
            escape(child_process),
            escape(&extra_json)
        )
    }

    /// CSV header
    pub fn csv_header() -> &'static str {
        "timestamp,session_id,host,shell_cwd,child_process,extra\n"
    }
}

/// Spawn a writer thread that handles session update records asynchronously
pub fn spawn_writer_thread(sessions_dir: PathBuf) -> mpsc::Sender<SessionUpdateRecord> {
    let (tx, rx): (mpsc::Sender<SessionUpdateRecord>, mpsc::Receiver<SessionUpdateRecord>) =
        mpsc::channel();

    thread::spawn(move || {
        // Ensure directory exists
        std::fs::create_dir_all(&sessions_dir).ok();

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
                        let path = sessions_dir.join(format!("{}.csv", current_date));
                        match std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            Ok(mut file) => {
                                // If file is empty, write header
                                if let Ok(metadata) = file.metadata() {
                                    if metadata.len() == 0 {
                                        let _ = file.write_all(SessionUpdateRecord::csv_header().as_bytes());
                                    }
                                }
                                writer = Some(Box::new(file));
                            }
                            Err(e) => {
                                tracing::error!("Failed to open sessions file: {}", e);
                                continue;
                            }
                        }
                    }

                    // Write the record
                    if let Some(ref mut w) = writer {
                        if let Err(e) = w.write_all(record.to_csv_row().as_bytes()) {
                            tracing::error!("Failed to write session update record: {}", e);
                        }
                        if let Err(e) = w.flush() {
                            tracing::error!("Failed to flush session update record: {}", e);
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
        let mut attrs = HashMap::new();
        attrs.insert("host".to_string(), "workstation".to_string());
        attrs.insert("shell_cwd".to_string(), "/home/user/project".to_string());
        attrs.insert("custom_field".to_string(), "custom_value".to_string());

        let record = SessionUpdateRecord::new(
            "test_session".to_string(),
            1709000000000,
            attrs,
        );

        let row = record.to_csv_row();
        // Check for readable timestamp format
        assert!(row.contains("2024-02-27"));
        assert!(row.contains("test_session"));
        assert!(row.contains("workstation"));
        assert!(row.contains("/home/user/project"));
        // custom_field should be in extra
        assert!(row.contains("custom_value"));
    }

    #[test]
    fn test_csv_header() {
        let header = SessionUpdateRecord::csv_header();
        assert!(header.starts_with("timestamp"));
        assert!(header.contains("session_id"));
        assert!(header.contains("host"));
        assert!(header.contains("shell_cwd"));
        assert!(header.contains("child_process"));
        assert!(header.contains("extra"));
    }

    #[test]
    fn test_extra_extraction() {
        let mut attrs = HashMap::new();
        attrs.insert("host".to_string(), "workstation".to_string());
        attrs.insert("shell_cwd".to_string(), "/home/user".to_string());
        attrs.insert("custom_key".to_string(), "custom_value".to_string());

        let record = SessionUpdateRecord::new(
            "test".to_string(),
            1000,
            attrs,
        );

        // host and shell_cwd should be extracted to explicit fields
        assert_eq!(record.host.as_deref(), Some("workstation"));
        assert_eq!(record.shell_cwd.as_deref(), Some("/home/user"));
        // custom_key should be extracted to extra
        assert!(record.extra.contains_key("custom_key"));
        assert_eq!(record.extra.get("custom_key").unwrap(), &Value::String("custom_value".to_string()));
    }

    #[test]
    fn test_writer_thread() {
        let dir = TempDir::new().unwrap();
        let tx = spawn_writer_thread(dir.path().to_path_buf());

        // Send a few records
        for i in 0..5 {
            let mut attrs = HashMap::new();
            attrs.insert("host".to_string(), "host".to_string());
            attrs.insert("shell_cwd".to_string(), format!("/path{}", i));
            attrs.insert("custom".to_string(), format!("val{}", i));
            let record = SessionUpdateRecord::new(
                format!("session{}", i),
                1709000000000 + i as u64,
                attrs,
            );
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
