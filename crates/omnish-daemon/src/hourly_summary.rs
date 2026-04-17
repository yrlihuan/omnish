use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use crate::task_mgr::{ScheduledTask, TaskContext};
use chrono::Local;
use omnish_common::config::ConfigMap;
use omnish_llm::{backend::{LlmBackend, LlmRequest, TriggerType, UseCase}, template};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_cron_scheduler::Job;

pub struct HourlySummaryTask {
    config: ConfigMap,
    schedule: String,
}

impl HourlySummaryTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(&config.get_string("schedule", ""));
        Self { config, schedule }
    }
}

impl ScheduledTask for HourlySummaryTask {
    fn name(&self) -> &'static str {
        "hourly_summary"
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    fn enabled(&self) -> bool {
        self.config.get_bool("enabled", true)
    }

    fn defaults() -> std::collections::HashMap<String, serde_json::Value> {
        [
            ("enabled".into(), serde_json::json!(true)),
            ("schedule".into(), serde_json::json!("0 */4 * * *")),
        ].into()
    }

    fn create_job(&self, ctx: &TaskContext) -> anyhow::Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let conv_mgr = ctx.conv_mgr.clone();
        let llm_holder = ctx.llm_backend.clone();
        let notes_dir = ctx.daemon.omnish_dir.join("notes");
        let daemon_config = ctx.daemon_config.clone();
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let mgr = mgr.clone();
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Analysis);
            let dir = notes_dir.clone();
            let language = daemon_config.read().unwrap().client.language.clone();
            Box::pin(async move {
                tracing::debug!("task [hourly_summary] started");
                if let Err(e) = generate_hourly_summary(&mgr, &conv_mgr, Some(llm.as_ref()), &dir, &language).await {
                    tracing::warn!("task [hourly_summary] failed: {}", e);
                }
                tracing::debug!("task [hourly_summary] finished");
            })
        })?)
    }
}

/// Build the LLM context for hourly/periodic summaries.
/// Returns `(context_for_llm, table_md)` - the table is reused when writing the output file.
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

/// Get section title in the given language.
fn section_title(section: &str, language: &str) -> &'static str {
    match (section, language) {
        ("commands", "zh") => "命令记录",
        ("conversations", "zh") => "会话记录",
        ("summary", "zh") => "工作总结",
        ("commands", "zh-tw") => "命令記錄",
        ("conversations", "zh-tw") => "會話記錄",
        ("summary", "zh-tw") => "工作總結",
        ("commands", "ja") => "コマンドログ",
        ("conversations", "ja") => "会話ログ",
        ("summary", "ja") => "作業まとめ",
        ("commands", "ko") => "명령 기록",
        ("conversations", "ko") => "대화 기록",
        ("summary", "ko") => "작업 요약",
        ("commands", "fr") => "Journal des commandes",
        ("conversations", "fr") => "Journal des conversations",
        ("summary", "fr") => "Résumé du travail",
        ("commands", "es") => "Registro de comandos",
        ("conversations", "es") => "Registro de conversaciones",
        ("summary", "es") => "Resumen del trabajo",
        ("commands", "ar") => "سجل الأوامر",
        ("conversations", "ar") => "سجل المحادثات",
        ("summary", "ar") => "ملخص العمل",
        ("commands", _) => "Command Log",
        ("conversations", _) => "Conversation Log",
        ("summary", _) => "Work Summary",
        (_, _) => "Work Summary", // fallback
    }
}

/// Format the main title for the hourly summary.
fn main_title(date_str: &str, hour_str: &str, language: &str) -> String {
    match language {
        "zh" => format!("{} {}:00 时工作摘要", date_str, hour_str),
        "zh-tw" => format!("{} {}:00 時工作摘要", date_str, hour_str),
        "ja" => format!("{} {}:00 作業サマリー", date_str, hour_str),
        "ko" => format!("{} {}:00 작업 요약", date_str, hour_str),
        "fr" => format!("Résumé horaire - {} {}:00", date_str, hour_str),
        "es" => format!("Resumen por hora - {} {}:00", date_str, hour_str),
        "ar" => format!("ملخص العمل - {} {}:00", date_str, hour_str),
        _ => format!("Hourly Work Summary - {} {}:00", date_str, hour_str),
    }
}

/// Get the table header row for the command log.
fn table_header(language: &str) -> &'static str {
    match language {
        "zh" => "| 时间 | 主机:工作目录 | 命令 |\n|------|--------------|------|",
        "zh-tw" => "| 時間 | 主機:工作目錄 | 命令 |\n|------|--------------|------|",
        "ja" => "| 時刻 | ホスト:作業ディレクトリ | コマンド |\n|------|--------------------------|---------|",
        "ko" => "| 시간 | 호스트:작업 디렉토리 | 명령 |\n|------|--------------------------|---------|",
        _ => "| Time | Host:Working Directory | Command |\n|------|--------------------------|---------|",
    }
}

/// Generate the hourly summary file with LLM summary.
async fn generate_hourly_summary(
    mgr: &SessionManager,
    conv_mgr: &ConversationManager,
    llm_backend: Option<&dyn LlmBackend>,
    summaries_dir: &Path,
    language: &str,
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

    let prompt = template::append_language_instruction(template::HOURLY_NOTES_PROMPT, language);

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
            system_prompt: None,
            enable_thinking: Some(true),
            tools: vec![],
            extra_messages: vec![],
        };
        match backend.complete(&req).await {
            Ok(resp) => Some(crate::strip_thinking_block(&resp.text())),
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
    let mut md = format!("# {}\n", main_title(&date_str, &hour_str, language));
    if !table_md.is_empty() {
        md.push_str(&format!("\n## {}\n", section_title("commands", language)));
        md.push_str(&format!("{}\n", table_header(language)));
        md.push_str(&table_md);
    }
    if !conversations_md.is_empty() {
        md.push_str(&format!("\n## {}\n\n", section_title("conversations", language)));
        md.push_str(&conversations_md);
    }
    md.push_str(&format!("\n## {}\n\n", section_title("summary", language)));
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
        generate_hourly_summary(&mgr, &conv_mgr, None, &summaries_dir, "en").await.unwrap();
        assert!(!summaries_dir.exists());
    }

    // Note: test with real command output requires proper stream file setup,
    // which is complex. The empty commands test verifies the skip logic.
}
