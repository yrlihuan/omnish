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

    // Try LLM summary
    if let Some(backend) = llm_backend {
        let table_text = &md;
        let use_case = UseCase::Analysis;
        let max_content_chars = backend.max_content_chars_for_use_case(use_case);
        let req = LlmRequest {
            context: table_text.clone(),
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
}
