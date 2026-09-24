use crate::conversation_mgr::ConversationManager;
use crate::session_mgr::SessionManager;
use crate::task_mgr::{ScheduledTask, TaskContext};
use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike};
use croner::Cron;
use omnish_common::config::ConfigMap;
use omnish_llm::{backend::{LlmBackend, LlmRequest, TriggerType, UseCase}, template};
use std::path::Path;
use tokio_cron_scheduler::Job;

pub struct HourlySummaryTask {
    config: ConfigMap,
    schedule: String,
}

impl HourlySummaryTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(&config.get_string("schedule", ""));
        // No 00:xx run means a day's last segment never lands in that day's
        // `24.md`, so the 00:10 daily_notes (already finished by the next fire)
        // cannot include it (issue #680 review).
        if !has_midnight_fire(&schedule) {
            tracing::warn!(
                "hourly_summary schedule '{}' has no 00:xx run; the last segment of each day \
                 (e.g. 20:00-24:00) will not appear in any daily note",
                schedule
            );
        }
        Self { config, schedule }
    }
}

/// Parse a normalized (6-field, seconds required) cron schedule in local time.
fn parse_schedule(schedule: &str) -> Option<Cron> {
    let mut cron = Cron::new(schedule);
    cron.with_seconds_required();
    cron.parse().ok()
}

/// Last scheduled fire time at or before `t`, or `None` when the schedule has
/// no occurrence within ~32 days. Uses the same croner parser as the scheduler,
/// so it matches actual firing for uneven schedules (weekday-only, hour lists,
/// `*/N`, etc.). Spans widen only until a hit is found, so a dense schedule is
/// not scanned for long.
pub fn last_fire_at_or_before(schedule: &str, t: DateTime<Local>) -> Option<DateTime<Local>> {
    let cron = parse_schedule(schedule)?;
    // 1h -> 25h -> 8d -> 32d
    for span_hours in [1_i64, 25, 8 * 24, 32 * 24] {
        let start = t - chrono::Duration::hours(span_hours);
        let mut last = None;
        for fire in cron.iter_from(start) {
            if fire > t {
                break;
            }
            last = Some(fire);
        }
        if last.is_some() {
            return last;
        }
    }
    None
}

/// Local date whose summary file a run at `now` writes: the 00:xx run saves the
/// previous day's `24.md`, every other run saves today.
fn summary_file_date(now: DateTime<Local>) -> NaiveDate {
    if now.format("%H").to_string() == "00" {
        (now - chrono::Duration::days(1)).date_naive()
    } else {
        now.date_naive()
    }
}

/// Clamp `t` up to local midnight of the summary file's date, so a window never
/// reaches into an earlier day (issue #680 review).
fn clamp_to_day_start(t: DateTime<Local>, now: DateTime<Local>) -> DateTime<Local> {
    let date = summary_file_date(now);
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .and_then(|naive| Local.from_local_datetime(&naive).single());
    match midnight {
        Some(midnight) => t.max(midnight),
        None => t,
    }
}

/// Window start for a scheduled run at `now`: the fire before the one at `now`.
/// The upper bound stays `now`, so the tiny gap between the scheduled fire and
/// actual wake-up is covered.
pub fn run_window_start(schedule: &str, now: DateTime<Local>) -> DateTime<Local> {
    let since = last_fire_at_or_before(schedule, now)
        .and_then(|this| last_fire_at_or_before(schedule, this - chrono::Duration::seconds(1)))
        .unwrap_or_else(|| now - chrono::Duration::hours(4));
    clamp_to_day_start(since, now)
}

/// Window start for the `/context hourly` preview: the most recent fire at or
/// before `now`, i.e. the part of the next run's window already elapsed.
pub fn preview_window_start(schedule: &str, now: DateTime<Local>) -> DateTime<Local> {
    let since = last_fire_at_or_before(schedule, now)
        .unwrap_or_else(|| now - chrono::Duration::hours(4));
    clamp_to_day_start(since, now)
}

/// Whether the schedule fires at least once during the 00:00-00:59 hour within
/// the next 25h. Unparseable schedules report `true` (do not warn).
fn has_midnight_fire(schedule: &str) -> bool {
    let Some(cron) = parse_schedule(schedule) else {
        return true;
    };
    let now = Local::now();
    let horizon = now + chrono::Duration::hours(25);
    cron.iter_after(now)
        .take_while(|fire| *fire <= horizon)
        .any(|fire| fire.hour() == 0)
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
        let schedule = self.schedule.clone();
        // Local timezone, matching `daily_notes`: an hour-aligned schedule must
        // fire on local day boundaries so the midnight run lands in the right
        // day's folder (issue #680 review).
        Ok(Job::new_async_tz(self.schedule(), Local, move |_uuid, _lock| {
            let mgr = mgr.clone();
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Analysis);
            let dir = notes_dir.clone();
            let language = daemon_config.read().unwrap().client.language.clone();
            let schedule = schedule.clone();
            Box::pin(async move {
                tracing::debug!("task [hourly_summary] started");
                let now = Local::now();
                if let Err(e) = generate_hourly_summary(&mgr, &conv_mgr, Some(llm.as_ref()), &dir, &language, &schedule, now).await {
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
///
/// The covered window starts at `run_window_start(schedule, now)` (the previous
/// scheduled fire, clamped to the local day) and ends at `now`, so a summary
/// reflects exactly the period since the last run.
async fn generate_hourly_summary(
    mgr: &SessionManager,
    conv_mgr: &ConversationManager,
    llm_backend: Option<&dyn LlmBackend>,
    summaries_dir: &Path,
    language: &str,
    schedule: &str,
    now: DateTime<Local>,
) -> anyhow::Result<()> {
    let since = run_window_start(schedule, now);
    let since_ms = since.timestamp_millis().max(0) as u64;

    let commands = mgr.collect_recent_commands(since_ms).await;
    let conversations_md = conv_mgr.collect_recent_conversations_md(since.into());

    if commands.is_empty() && conversations_md.is_empty() {
        tracing::info!(
            "hourly summary: no commands or conversations since {}, skipping",
            since.format("%Y-%m-%d %H:%M")
        );
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
        match crate::llm_pool::complete(backend, &req).await {
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

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[tokio::test]
    async fn test_generate_hourly_summary_empty_commands() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let summaries_dir = dir.path().join("summaries");

        // No commands or conversations -> should skip without error
        generate_hourly_summary(
            &mgr,
            &conv_mgr,
            None,
            &summaries_dir,
            "en",
            "0 0 */4 * * *",
            Local::now(),
        )
        .await
        .unwrap();
        assert!(!summaries_dir.exists());
    }

    #[test]
    fn test_last_fire_at_or_before_uneven_schedules() {
        // Default every-4h: 2024-01-15 03:30 -> previous fire 00:00.
        assert_eq!(
            last_fire_at_or_before("0 0 */4 * * *", dt(2024, 1, 15, 3, 30)),
            Some(dt(2024, 1, 15, 0, 0))
        );
        // Hour list 8,12,20 at 15:00 -> 12:00 (8h gap), at 07:00 -> yesterday 20:00.
        assert_eq!(
            last_fire_at_or_before("0 0 8,12,20 * * *", dt(2024, 1, 15, 15, 0)),
            Some(dt(2024, 1, 15, 12, 0))
        );
        assert_eq!(
            last_fire_at_or_before("0 0 8,12,20 * * *", dt(2024, 1, 15, 7, 0)),
            Some(dt(2024, 1, 14, 20, 0))
        );
        // */5 has a 4h gap on the 20:00 -> 00:00 boundary.
        assert_eq!(
            last_fire_at_or_before("0 0 */5 * * *", dt(2024, 1, 14, 23, 0)),
            Some(dt(2024, 1, 14, 20, 0))
        );
        assert_eq!(
            last_fire_at_or_before("0 0 */5 * * *", dt(2024, 1, 15, 2, 0)),
            Some(dt(2024, 1, 15, 0, 0))
        );
        // Weekday-only 8,12,20 (Mon 2024-01-15 00:00 -> Friday 2024-01-12 20:00).
        assert_eq!(
            last_fire_at_or_before("0 0 8,12,20 * * 1-5", dt(2024, 1, 15, 0, 0)),
            Some(dt(2024, 1, 12, 20, 0))
        );
        // Hourly: previous fire is the top of the hour.
        assert_eq!(
            last_fire_at_or_before("0 0 * * * *", dt(2024, 1, 15, 10, 30)),
            Some(dt(2024, 1, 15, 10, 0))
        );
        // Single fixed hour: previous fire is that hour, same day.
        assert_eq!(
            last_fire_at_or_before("0 0 3 * * *", dt(2024, 1, 15, 5, 0)),
            Some(dt(2024, 1, 15, 3, 0))
        );
    }

    #[test]
    fn test_run_window_start_default_and_midnight_clamp() {
        // Default every-4h at 04:00: window starts at 00:00 same day.
        assert_eq!(
            run_window_start("0 0 */4 * * *", dt(2024, 1, 15, 4, 0)),
            dt(2024, 1, 15, 0, 0)
        );
        // 00:00 run writes the previous day's 24.md; since = 20:00 prev day,
        // and must NOT be clamped to today (which would empty the window).
        assert_eq!(
            run_window_start("0 0 */4 * * *", dt(2024, 1, 15, 0, 0)),
            dt(2024, 1, 14, 20, 0)
        );
        // 8,12,20 at 08:00: previous fire is yesterday 20:00, clamped to today 00:00.
        assert_eq!(
            run_window_start("0 0 8,12,20 * * *", dt(2024, 1, 15, 8, 0)),
            dt(2024, 1, 15, 0, 0)
        );
        // 8,12,20 at 20:00: previous fire 12:00 same day, no clamp needed.
        assert_eq!(
            run_window_start("0 0 8,12,20 * * *", dt(2024, 1, 15, 20, 0)),
            dt(2024, 1, 15, 12, 0)
        );
    }

    #[test]
    fn test_preview_window_start() {
        // Preview at 06:00 with every-4h: the next run covers from 04:00.
        assert_eq!(
            preview_window_start("0 0 */4 * * *", dt(2024, 1, 15, 6, 0)),
            dt(2024, 1, 15, 4, 0)
        );
    }

    #[test]
    fn test_has_midnight_fire() {
        assert!(has_midnight_fire("0 0 */4 * * *"));
        assert!(has_midnight_fire("0 0 */5 * * *"));
        assert!(has_midnight_fire("0 0 * * * *"));
        assert!(!has_midnight_fire("0 0 8,12,20 * * *"));
        assert!(!has_midnight_fire("0 0 9-17 * * *"));
    }

    // Note: test with real command output requires proper stream file setup,
    // which is complex. The empty commands test verifies the skip logic.
}
