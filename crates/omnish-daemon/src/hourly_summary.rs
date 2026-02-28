use crate::session_mgr::SessionManager;
use chrono::Local;
use omnish_llm::backend::{LlmBackend, UseCase};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_cron_scheduler::Job;

/// Create a cron job that generates hourly summaries.
pub fn create_hourly_summary_job(
    mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    summaries_dir: PathBuf,
) -> anyhow::Result<Job> {
    // Run every hour at minute 0
    let cron = "0 0 * * * *".to_string();
    Ok(Job::new_async(cron, move |_uuid, _lock| {
        let mgr = mgr.clone();
        let llm = llm_backend.clone();
        let dir = summaries_dir.clone();
        Box::pin(async move {
            if let Err(e) = generate_hourly_summary(&mgr, llm.as_deref(), &dir).await {
                tracing::warn!("hourly summary generation failed: {}", e);
            }
        })
    })?)
}

/// Generate the hourly summary context file.
async fn generate_hourly_summary(
    mgr: &SessionManager,
    llm_backend: Option<&dyn LlmBackend>,
    summaries_dir: &PathBuf,
) -> anyhow::Result<()> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let one_hour_ms = 3600 * 1000;
    let since_ms = now_ms.saturating_sub(one_hour_ms);

    let commands = mgr.collect_recent_commands(since_ms).await;
    if commands.is_empty() {
        tracing::info!("hourly summary: no commands in the last hour, skipping");
        return Ok(());
    }

    // Get max_content_chars from analysis use case
    let max_content_chars = llm_backend
        .map(|b| b.max_content_chars_for_use_case(UseCase::Analysis))
        .unwrap_or(None);

    // Get hourly summary config
    let config = mgr.get_hourly_summary_config();

    // Build context using hourly summary config
    let context = mgr
        .build_hourly_summary_context(&commands, max_content_chars, &config)
        .await?;

    if context.is_empty() {
        tracing::info!("hourly summary: context is empty after building, skipping");
        return Ok(());
    }

    // Generate filename: YYYY-MM-DD-HH.md
    let now = Local::now();
    let filename = format!("{}.md", now.format("%Y-%m-%d-%H"));
    let file_path = summaries_dir.join(&filename);

    // Build markdown content
    let mut md = format!("# {} 时工作摘要\n\n", now.format("%Y-%m-%d %H:00"));
    md.push_str(&context);

    // Write file
    std::fs::create_dir_all(summaries_dir)?;
    std::fs::write(&file_path, &md)?;
    tracing::info!("hourly summary: wrote {}", file_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_hourly_summary_empty_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let summaries_dir = dir.path().join("summaries");

        // No commands -> should skip without error
        generate_hourly_summary(&mgr, None, &summaries_dir).await.unwrap();
        assert!(!summaries_dir.exists());
    }

    #[tokio::test]
    async fn test_generate_hourly_summary_with_commands() {
        use omnish_common::config::{ContextConfig, HourlySummaryConfig};
        use omnish_store::command::CommandRecord;

        let dir = tempfile::tempdir().unwrap();
        let config = ContextConfig {
            hourly_summary: HourlySummaryConfig {
                head_lines: 10,
                tail_lines: 10,
                max_line_width: 128,
            },
            ..Default::default()
        };
        let mgr = SessionManager::new(dir.path().to_path_buf(), config);

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

        let summaries_dir = dir.path().join("summaries");
        generate_hourly_summary(&mgr, None, &summaries_dir).await.unwrap();

        let now = Local::now();
        let filename = format!("{}.md", now.format("%Y-%m-%d-%H"));
        let content = std::fs::read_to_string(summaries_dir.join(&filename)).unwrap();
        assert!(content.contains("工作摘要"));
        assert!(content.contains("cargo build"));
    }
}
