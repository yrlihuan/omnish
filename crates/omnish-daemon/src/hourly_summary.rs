use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use crate::task_mgr::{ScheduledTask, TaskContext};
use chrono::Local;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType, UseCase};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_cron_scheduler::Job;

pub struct HourlySummaryTask(pub omnish_common::config::HourlySummaryConfig);

impl ScheduledTask for HourlySummaryTask {
    fn name(&self) -> &'static str {
        "hourly_summary"
    }

    fn schedule(&self) -> &str {
        "0 0 */4 * * *"
    }

    fn enabled(&self) -> bool {
        self.0.enabled
    }

    fn create_job(&self, ctx: &TaskContext) -> anyhow::Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let conv_mgr = ctx.conv_mgr.clone();
        let llm_holder = ctx.llm_backend.clone();
        let notes_dir = ctx.daemon.omnish_dir.join("notes");
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Analysis);
            let dir = notes_dir.clone();
            Box::pin(async move {
                tracing::debug!("task [hourly_summary] started");
                if let Err(e) = generate_hourly_summary(&mgr, &conv_mgr, Some(llm.as_ref()), &dir).await {
                    tracing::warn!("task [hourly_summary] failed: {}", e);
                }
                tracing::debug!("task [hourly_summary] finished");
            })
        })?)
    }
}

/// Build the LLM context for hourly/periodic summaries.
/// Returns `(context_for_llm, table_md)` — the table is reused when writing the output file.
/// Used by both the scheduled job and `/context hourly-notes`.
pub fn build_hourly_context(
    commands: &[(String, omnish_store::command::CommandRecord)],
    conversations_md: &str,
) -> (String, String) {
    let mut table_md = String::new();
    for (hostname, cmd) in commands {
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

    let mut context = String::new();
    if !table_md.is_empty() {
        context.push_str(&format!("<commands>\n{}</commands>\n\n", table_md));
    }
    if !conversations_md.is_empty() {
        context.push_str(&format!("<conversations>\n{}</conversations>", conversations_md));
    }

    (context, table_md)
}

/// Generate the hourly summary file with LLM summary.
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
    let since_ms = now_ms.saturating_sub(4 * 3600 * 1000);

    let commands = mgr.collect_recent_commands(since_ms).await;
    let window_ago = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(4 * 3600))
        .unwrap_or(UNIX_EPOCH);
    let conversations_md = conv_mgr.collect_recent_conversations_md(window_ago);

    if commands.is_empty() && conversations_md.is_empty() {
        tracing::info!("hourly summary: no commands or conversations in the last 4 hours, skipping");
        return Ok(());
    }

    let (context, table_md) = build_hourly_context(&commands, &conversations_md);

    let prompt = "以下<commands>中是从多台终端收集的过去4小时的命令及其简要输出（如有），\
         <conversations>中是与AI助手的对话记录（如有）。\
         请用中文以项目符号列表形式列出这4小时的工作内容，每个条目包含一项主要活动或成果。适合直接作为工作日志。".to_string();

    // Try LLM summary
    let summary = if let Some(backend) = llm_backend {
        let use_case = UseCase::Analysis;
        let max_content_chars = backend.max_content_chars();
        let req = LlmRequest {
            context,
            query: Some(prompt),
            trigger: TriggerType::AutoPattern,
            session_ids: vec![],
            use_case,
            max_content_chars,
            conversation: vec![],
            system_prompt: None,
            enable_thinking: Some(true),
            tools: vec![],
            extra_messages: vec![],
        };
        match backend.complete(&req).await {
            Ok(resp) => Some(resp.text()),
            Err(e) => {
                tracing::warn!("periodic summary: LLM summary failed, skipping: {}", e);
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
    // Special case: at midnight (00:xx), save as previous day's 24.md
    // so that daily notes (running at 00:10) can include this last summary.
    let now = Local::now();
    let (date_str, hour_str) = if now.format("%H").to_string() == "00" {
        let yesterday = now - chrono::Duration::days(1);
        (yesterday.format("%Y-%m-%d").to_string(), "24".to_string())
    } else {
        (now.format("%Y-%m-%d").to_string(), now.format("%H").to_string())
    };
    let date_dir = summaries_dir.join("hourly").join(&date_str);
    let filename = format!("{}.md", hour_str);
    let file_path = date_dir.join(&filename);

    // Build markdown content: commands + conversations + LLM summary
    let mut md = format!("# {} {}:00 时工作摘要\n", date_str, hour_str);
    if !table_md.is_empty() {
        md.push_str("\n## 命令记录\n");
        md.push_str("| 时间 | 主机:工作目录 | 命令 |\n");
        md.push_str("|------|--------------|------|\n");
        md.push_str(&table_md);
    }
    if !conversations_md.is_empty() {
        md.push_str("\n## 会话记录\n\n");
        md.push_str(&conversations_md);
    }
    md.push_str("\n## 工作总结\n\n");
    md.push_str(&summary);

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
