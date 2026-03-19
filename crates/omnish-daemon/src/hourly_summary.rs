use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use chrono::Local;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType, UseCase};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_cron_scheduler::Job;

/// Create a cron job that generates hourly summaries.
pub fn create_hourly_summary_job(
    mgr: Arc<SessionManager>,
    conv_mgr: Arc<ConversationManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    summaries_dir: PathBuf,
) -> anyhow::Result<Job> {
    // Run every hour at minute 0
    let cron = "0 0 * * * *".to_string();
    Ok(Job::new_async(cron, move |_uuid, _lock| {
        let mgr = mgr.clone();
        let conv_mgr = conv_mgr.clone();
        let llm = llm_backend.clone();
        let dir = summaries_dir.clone();
        Box::pin(async move {
            tracing::debug!("task [hourly_summary] started");
            if let Err(e) = generate_hourly_summary(&mgr, &conv_mgr, llm.as_deref(), &dir).await {
                tracing::warn!("task [hourly_summary] failed: {}", e);
            }
            tracing::debug!("task [hourly_summary] finished");
        })
    })?)
}

/// Generate the hourly summary file with LLM summary only.
async fn generate_hourly_summary(
    mgr: &SessionManager,
    conv_mgr: &ConversationManager,
    llm_backend: Option<&dyn LlmBackend>,
    summaries_dir: &Path,
) -> anyhow::Result<()> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let one_hour_ms = 3600 * 1000;
    let since_ms = now_ms.saturating_sub(one_hour_ms);

    let commands = mgr.collect_recent_commands(since_ms).await;
    let one_hour_ago = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(3600))
        .unwrap_or(UNIX_EPOCH);
    let conversations_md = conv_mgr.collect_recent_conversations_md(one_hour_ago);

    if commands.is_empty() && conversations_md.is_empty() {
        tracing::info!("hourly summary: no commands or conversations in the last hour, skipping");
        return Ok(());
    }

    // Build the markdown table for LLM context
    let mut table_md = String::new();
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
        let command_line = command_line.replace('|', "\\|");
        table_md.push_str(&format!(
            "| {} | {}:{} | {} |\n",
            time, host, cwd, command_line
        ));
    }

    // Build context with commands and conversations
    let mut context = String::new();
    if !table_md.is_empty() {
        context.push_str(&format!("<commands>\n{}</commands>\n\n", table_md));
    }
    if !conversations_md.is_empty() {
        context.push_str(&format!("<conversations>\n{}</conversations>", conversations_md));
    }

    // Try LLM summary
    let summary = if let Some(backend) = llm_backend {
        let use_case = UseCase::Analysis;
        let max_content_chars = backend.max_content_chars_for_use_case(use_case);
        let req = LlmRequest {
            context,
            query: Some(omnish_llm::template::HOURLY_NOTES_PROMPT.to_string()),
            trigger: TriggerType::AutoPattern,
            session_ids: vec![],
            use_case,
            max_content_chars,
            conversation: vec![],
            system_prompt: None,
            enable_thinking: None, // Use default (thinking enabled for analysis)
            tools: vec![],
            extra_messages: vec![],
        };
        match backend.complete(&req).await {
            Ok(resp) => Some(resp.text()),
            Err(e) => {
                tracing::warn!("hourly summary: LLM summary failed, skipping: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Skip if no LLM available
    let summary = match summary {
        Some(s) => s,
        None => {
            tracing::info!("hourly summary: no LLM available, skipping");
            return Ok(());
        }
    };

    // Generate filename: notes/hourly/YYYY-MM-DD/HH.md
    let now = Local::now();
    let date_dir = summaries_dir.join("hourly").join(now.format("%Y-%m-%d").to_string());
    let filename = format!("{}.md", now.format("%H"));
    let file_path = date_dir.join(&filename);

    // Build markdown content - only the LLM summary
    let md = format!("# {} 时工作摘要\n\n{}", now.format("%Y-%m-%d %H:00"), summary);

    // Write file
    std::fs::create_dir_all(&date_dir)?;
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
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let summaries_dir = dir.path().join("summaries");

        // No commands or conversations -> should skip without error
        generate_hourly_summary(&mgr, &conv_mgr, None, &summaries_dir).await.unwrap();
        assert!(!summaries_dir.exists());
    }

    // Note: test with real command output requires proper stream file setup,
    // which is complex. The empty commands test verifies the skip logic.
}
