use crate::session_mgr::SessionManager;
use chrono::Local;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType, UseCase};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_cron_scheduler::Job;

/// Create a cron job that generates daily notes at the given hour.
pub fn create_daily_notes_job(
    mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    notes_dir: PathBuf,
    schedule_hour: u8,
) -> anyhow::Result<Job> {
    let cron = format!("0 0 {} * * *", schedule_hour);
    Ok(Job::new_async(cron, move |_uuid, _lock| {
        let mgr = mgr.clone();
        let llm = llm_backend.clone();
        let dir = notes_dir.clone();
        Box::pin(async move {
            tracing::debug!("task [daily_notes] started");
            if let Err(e) = generate_daily_note(&mgr, llm.as_deref(), &dir).await {
                tracing::warn!("task [daily_notes] failed: {}", e);
            }
            tracing::debug!("task [daily_notes] finished");
        })
    })?)
}

/// Generate the daily note markdown file.
async fn generate_daily_note(
    mgr: &SessionManager,
    llm_backend: Option<&dyn LlmBackend>,
    notes_dir: &PathBuf,
) -> anyhow::Result<()> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let twenty_four_hours_ms = 24 * 3600 * 1000;
    let since_ms = now_ms.saturating_sub(twenty_four_hours_ms);

    let commands = mgr.collect_recent_commands(since_ms).await;
    if commands.is_empty() {
        tracing::info!("daily notes: no commands in the last 24h, skipping");
        return Ok(());
    }

    let today = Local::now().format("%Y-%m-%d").to_string();

    // Build the markdown table
    let mut md = format!("# {} 工作日报\n\n## 命令记录\n", today);
    md.push_str("| 时间 | 主机:工作目录 | 命令 |\n");
    md.push_str("|------|--------------|------|\n");

    for (hostname, cmd) in &commands {
        let time = {
            let dt = chrono::DateTime::from_timestamp_millis(cmd.started_at as i64)
                .unwrap_or_default()
                .with_timezone(&Local);
            dt.format("%H:%M").to_string()
        };
        let host = if hostname.is_empty() { "?" } else { hostname };
        let cwd = cmd.cwd.as_deref().unwrap_or("?");
        let command_line = cmd.command_line.as_deref().unwrap_or("?");
        // Escape pipes in markdown table cells
        let command_line = command_line.replace('|', "\\|");
        md.push_str(&format!(
            "| {} | {}:{} | {} |\n",
            time, host, cwd, command_line
        ));
    }

    // Collect hourly notes for today as additional context
    let hourly_context = collect_hourly_notes(notes_dir, &today);

    // Try LLM summary
    if let Some(backend) = llm_backend {
        let mut context = md.clone();
        if !hourly_context.is_empty() {
            context.push_str("\n## 每小时工作摘要\n\n");
            context.push_str(&hourly_context);
        }
        let use_case = UseCase::Analysis;
        let max_content_chars = backend.max_content_chars_for_use_case(use_case);
        let req = LlmRequest {
            context,
            query: Some(omnish_llm::template::DAILY_NOTES_PROMPT.to_string()),
            trigger: TriggerType::AutoPattern,
            session_ids: vec![],
            use_case,
            max_content_chars,
        };
        match backend.complete(&req).await {
            Ok(resp) => {
                md.push_str("\n## 工作总结\n");
                md.push_str(&resp.content);
                md.push('\n');
            }
            Err(e) => {
                tracing::warn!("daily notes: LLM summary failed, skipping: {}", e);
            }
        }
    }

    // Write file
    std::fs::create_dir_all(notes_dir)?;
    let file_path = notes_dir.join(format!("{}.md", today));
    std::fs::write(&file_path, &md)?;
    tracing::info!("daily notes: wrote {}", file_path.display());

    Ok(())
}

/// Read all hourly note files for the given date (YYYY-MM-DD) and concatenate them.
fn collect_hourly_notes(notes_dir: &PathBuf, date: &str) -> String {
    let hourly_dir = notes_dir.join("hourly").join(date);
    let mut entries: Vec<_> = match std::fs::read_dir(&hourly_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map_or(false, |ext| ext == "md")
            })
            .collect(),
        Err(_) => return String::new(),
    };
    entries.sort_by_key(|e| e.file_name());

    let mut result = String::new();
    for entry in entries {
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            result.push_str(&content);
            result.push_str("\n\n");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_daily_note_empty_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let notes_dir = dir.path().join("notes");

        // No commands → should skip without error
        generate_daily_note(&mgr, None, &notes_dir).await.unwrap();
        assert!(!notes_dir.exists());
    }

    #[tokio::test]
    async fn test_generate_daily_note_with_commands() {
        use omnish_store::command::CommandRecord;

        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());

        let mut attrs = std::collections::HashMap::new();
        attrs.insert("hostname".to_string(), "dev-server".to_string());
        mgr.register("s1", None, attrs).await.unwrap();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        mgr.receive_command(
            "s1",
            CommandRecord {
                command_id: "c1".into(),
                session_id: "s1".into(),
                command_line: Some("cargo build".into()),
                cwd: Some("/home/user/project".into()),
                started_at: now_ms - 1000,
                ended_at: Some(now_ms),
                output_summary: String::new(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: Some(0),
            },
        )
        .await
        .unwrap();

        let notes_dir = dir.path().join("notes");
        generate_daily_note(&mgr, None, &notes_dir).await.unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let content = std::fs::read_to_string(notes_dir.join(format!("{}.md", today))).unwrap();
        assert!(content.contains("工作日报"));
        assert!(content.contains("cargo build"));
        assert!(content.contains("dev-server"));
        assert!(content.contains("/home/user/project"));
        // No LLM → no summary section
        assert!(!content.contains("工作总结"));
    }

    #[tokio::test]
    async fn test_generate_daily_note_with_mock_llm() {
        use async_trait::async_trait;
        use omnish_llm::backend::{LlmBackend, LlmRequest, LlmResponse};
        use omnish_store::command::CommandRecord;

        struct MockLlm;

        #[async_trait]
        impl LlmBackend for MockLlm {
            async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
                Ok(LlmResponse {
                    content: "今天主要进行了项目构建工作。".to_string(),
                    model: "mock".to_string(),
                    thinking: None,
                })
            }
            fn name(&self) -> &str {
                "mock"
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());

        let mut attrs = std::collections::HashMap::new();
        attrs.insert("hostname".to_string(), "myhost".to_string());
        mgr.register("s1", None, attrs).await.unwrap();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        mgr.receive_command(
            "s1",
            CommandRecord {
                command_id: "c1".into(),
                session_id: "s1".into(),
                command_line: Some("git pull".into()),
                cwd: Some("/home/user/repo".into()),
                started_at: now_ms - 500,
                ended_at: Some(now_ms),
                output_summary: String::new(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: Some(0),
            },
        )
        .await
        .unwrap();

        let notes_dir = dir.path().join("notes");
        let mock_llm: &dyn LlmBackend = &MockLlm;
        generate_daily_note(&mgr, Some(mock_llm), &notes_dir)
            .await
            .unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let content = std::fs::read_to_string(notes_dir.join(format!("{}.md", today))).unwrap();
        assert!(content.contains("工作总结"));
        assert!(content.contains("今天主要进行了项目构建工作。"));
    }

    #[test]
    fn test_collect_hourly_notes() {
        let dir = tempfile::tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        let hourly_dir = notes_dir.join("hourly").join("2026-03-01");
        std::fs::create_dir_all(&hourly_dir).unwrap();

        std::fs::write(hourly_dir.join("09.md"), "# 09:00 摘要\n上午工作").unwrap();
        std::fs::write(hourly_dir.join("10.md"), "# 10:00 摘要\n继续工作").unwrap();

        let result = collect_hourly_notes(&notes_dir, "2026-03-01");
        assert!(result.contains("09:00 摘要"));
        assert!(result.contains("10:00 摘要"));
        assert!(result.contains("上午工作"));
        assert!(result.contains("继续工作"));
        // Should be sorted (09 before 10)
        let pos_09 = result.find("09:00").unwrap();
        let pos_10 = result.find("10:00").unwrap();
        assert!(pos_09 < pos_10);
    }

    #[test]
    fn test_collect_hourly_notes_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let notes_dir = dir.path().join("notes");
        let result = collect_hourly_notes(&notes_dir, "2026-03-01");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_daily_note_includes_hourly_context_for_llm() {
        use async_trait::async_trait;
        use omnish_llm::backend::{LlmBackend, LlmRequest, LlmResponse};
        use omnish_store::command::CommandRecord;
        use std::sync::Mutex;

        struct CaptureLlm {
            captured_context: Mutex<String>,
        }

        #[async_trait]
        impl LlmBackend for CaptureLlm {
            async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
                *self.captured_context.lock().unwrap() = req.context.clone();
                Ok(LlmResponse {
                    content: "总结".to_string(),
                    model: "mock".to_string(),
                    thinking: None,
                })
            }
            fn name(&self) -> &str {
                "capture"
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());

        let mut attrs = std::collections::HashMap::new();
        attrs.insert("hostname".to_string(), "host".to_string());
        mgr.register("s1", None, attrs).await.unwrap();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        mgr.receive_command(
            "s1",
            CommandRecord {
                command_id: "c1".into(),
                session_id: "s1".into(),
                command_line: Some("echo test".into()),
                cwd: Some("/tmp".into()),
                started_at: now_ms - 100,
                ended_at: Some(now_ms),
                output_summary: String::new(),
                stream_offset: 0,
                stream_length: 0,
                exit_code: Some(0),
            },
        )
        .await
        .unwrap();

        // Create hourly notes for today
        let notes_dir = dir.path().join("notes");
        let today = Local::now().format("%Y-%m-%d").to_string();
        let hourly_dir = notes_dir.join("hourly").join(&today);
        std::fs::create_dir_all(&hourly_dir).unwrap();
        std::fs::write(hourly_dir.join("14.md"), "# 14:00 摘要\n下午工作内容").unwrap();

        let capture_llm = Arc::new(CaptureLlm {
            captured_context: Mutex::new(String::new()),
        });
        let llm_ref: &dyn LlmBackend = capture_llm.as_ref();
        generate_daily_note(&mgr, Some(llm_ref), &notes_dir)
            .await
            .unwrap();

        let ctx = capture_llm.captured_context.lock().unwrap();
        assert!(ctx.contains("每小时工作摘要"), "LLM context should include hourly section");
        assert!(ctx.contains("下午工作内容"), "LLM context should include hourly note content");
    }
}
