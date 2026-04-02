use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use crate::task_mgr::{ScheduledTask, TaskContext};
use chrono::Local;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType, UseCase};
use std::path::{Path, PathBuf};
use tokio_cron_scheduler::Job;

pub struct DailyNotesTask(pub omnish_common::config::DailyNotesConfig);

impl ScheduledTask for DailyNotesTask {
    fn name(&self) -> &'static str {
        "daily_notes"
    }

    fn schedule(&self) -> &str {
        "0 10 0 * * *"
    }

    fn enabled(&self) -> bool {
        self.0.enabled
    }

    fn create_job(&self, ctx: &TaskContext) -> anyhow::Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let conv_mgr = ctx.conv_mgr.clone();
        let llm_holder = ctx.llm_backend.clone();
        let notes_dir = ctx.daemon.omnish_dir.join("notes");
        Ok(Job::new_async_tz(self.schedule(), Local, move |_uuid, _lock| {
            let mgr = mgr.clone();
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Analysis);
            let dir = notes_dir.clone();
            Box::pin(async move {
                tracing::debug!("task [daily_notes] started");
                if let Err(e) = generate_daily_note(&mgr, &conv_mgr, Some(llm.as_ref()), &dir).await {
                    tracing::warn!("task [daily_notes] failed: {}", e);
                }
                tracing::debug!("task [daily_notes] finished");
            })
        })?)
    }
}

/// Build the LLM context for daily notes: collects hourly summaries for the given date.
/// Used by both the scheduled job and `/context daily-notes`.
pub fn build_daily_context(notes_dir: &Path, date: &str) -> String {
    let hourly_context = collect_hourly_notes(notes_dir, date);
    if hourly_context.is_empty() {
        return String::new();
    }
    format!("<hourly_summaries>\n{}</hourly_summaries>", hourly_context)
}

/// Read all hourly note files for the given date (YYYY-MM-DD) and concatenate them.
fn collect_hourly_notes(notes_dir: &Path, date: &str) -> String {
    let hourly_dir = notes_dir.join("hourly").join(date);
    let mut entries: Vec<_> = match std::fs::read_dir(&hourly_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "md")
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

/// Generate the daily note markdown file.
async fn generate_daily_note(
    _mgr: &SessionManager,
    _conv_mgr: &ConversationManager,
    llm_backend: Option<&dyn LlmBackend>,
    notes_dir: &PathBuf,
) -> anyhow::Result<()> {
    // Daily notes runs at 00:10 and summarizes the previous day
    let yesterday = (Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();

    // Daily notes relies entirely on hourly summaries (which contain commands + conversations)
    let summary = if let Some(backend) = llm_backend {
        let use_case = UseCase::Analysis;
        let max_content_chars = backend.max_content_chars();

        let llm_context = build_daily_context(notes_dir, &yesterday);
        if llm_context.is_empty() {
            tracing::info!("daily notes: no hourly summaries for {}, skipping", yesterday);
            return Ok(());
        }

        let req = LlmRequest {
            context: llm_context,
            query: Some(omnish_llm::template::DAILY_NOTES_PROMPT.to_string()),
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
                tracing::warn!("daily notes: LLM summary failed, skipping: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Skip writing if no LLM summary available
    let summary = match summary {
        Some(s) => s,
        None => {
            tracing::info!("daily notes: no LLM available, skipping file write");
            return Ok(());
        }
    };

    // Write file — only the LLM summary, no raw commands/conversations
    let md = format!("# {} 工作日报\n\n{}\n", yesterday, summary);
    std::fs::create_dir_all(notes_dir)?;
    let file_path = notes_dir.join(format!("{}.md", yesterday));
    std::fs::write(&file_path, &md)?;
    tracing::info!("daily notes: wrote {}", file_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a fake hourly summary file for yesterday.
    fn write_hourly_file(notes_dir: &Path, hour: &str, content: &str) {
        let yesterday = (Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        let hourly_dir = notes_dir.join("hourly").join(&yesterday);
        std::fs::create_dir_all(&hourly_dir).unwrap();
        std::fs::write(hourly_dir.join(format!("{}.md", hour)), content).unwrap();
    }

    #[tokio::test]
    async fn test_generate_daily_note_no_hourly_summaries() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let notes_dir = dir.path().join("notes");

        // No hourly summaries → should skip without error
        generate_daily_note(&mgr, &conv_mgr, None, &notes_dir).await.unwrap();
        let yesterday = (Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        assert!(!notes_dir.join(format!("{}.md", yesterday)).exists());
    }

    #[tokio::test]
    async fn test_generate_daily_note_no_llm() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let notes_dir = dir.path().join("notes");

        // Create hourly summary but no LLM → should skip file write
        write_hourly_file(&notes_dir, "10", "# 2026-03-30 10:00 时工作摘要\n\n- did stuff");
        generate_daily_note(&mgr, &conv_mgr, None, &notes_dir).await.unwrap();
        let yesterday = (Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        assert!(!notes_dir.join(format!("{}.md", yesterday)).exists());
    }

    #[tokio::test]
    async fn test_generate_daily_note_with_mock_llm() {
        use async_trait::async_trait;
        use omnish_llm::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason};

        struct MockLlm;

        #[async_trait]
        impl LlmBackend for MockLlm {
            async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
                Ok(LlmResponse {
                    content: vec![ContentBlock::Text("今天主要进行了项目构建工作。".to_string())],
                    stop_reason: StopReason::EndTurn,
                    model: "mock".to_string(),
                    usage: None,
                })
            }
            fn name(&self) -> &str {
                "mock"
            }
            fn model_name(&self) -> &str {
                "mock-model"
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let notes_dir = dir.path().join("notes");

        // Create hourly summaries
        write_hourly_file(&notes_dir, "10", "# 10:00 摘要\n\n## 命令记录\n| 10:05 | host:/proj | cargo build |\n\n## 工作总结\n\n- 编译项目");
        write_hourly_file(&notes_dir, "14", "# 14:00 摘要\n\n## 命令记录\n| 14:30 | host:/proj | git push |\n\n## 工作总结\n\n- 推送代码");

        let mock_llm: &dyn LlmBackend = &MockLlm;
        generate_daily_note(&mgr, &conv_mgr, Some(mock_llm), &notes_dir)
            .await
            .unwrap();

        let yesterday = (Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        let content = std::fs::read_to_string(notes_dir.join(format!("{}.md", yesterday))).unwrap();
        assert!(content.contains("工作日报"));
        assert!(content.contains("今天主要进行了项目构建工作。"));
        // Daily notes should only contain LLM summary, not raw hourly content
        assert!(!content.contains("cargo build"));
        assert!(!content.contains("git push"));
    }
}
