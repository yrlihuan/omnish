use crate::conversation_mgr::ConversationManager;
use crate::task_mgr::{ScheduledTask, TaskContext};
use omnish_common::config::ConfigMap;
use omnish_llm::{backend::{LlmBackend, LlmRequest, TriggerType, UseCase}, template};
use tokio_cron_scheduler::Job;


pub struct ThreadSummaryTask {
    config: ConfigMap,
    schedule: String,
}

impl ThreadSummaryTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = crate::task_mgr::normalize_cron(&config.get_string("schedule", ""));
        Self { config, schedule }
    }
}

impl ScheduledTask for ThreadSummaryTask {
    fn name(&self) -> &'static str {
        "thread_summary"
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
            ("schedule".into(), serde_json::json!("* * * * *")),
        ].into()
    }

    fn create_job(&self, ctx: &TaskContext) -> anyhow::Result<Job> {
        let conv_mgr = ctx.conv_mgr.clone();
        let llm_holder = ctx.llm_backend.clone();
        let daemon_config = ctx.daemon_config.clone();
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let conv_mgr = conv_mgr.clone();
            let llm = llm_holder.read().unwrap().get_backend(UseCase::Chat);
            let language = daemon_config.read().unwrap().client.language.clone();
            Box::pin(async move {
                tracing::debug!("task [thread_summary] started");
                if let Err(e) = generate_thread_summaries(&conv_mgr, Some(llm.as_ref()), &language).await {
                    tracing::warn!("task [thread_summary] failed: {}", e);
                }
                tracing::debug!("task [thread_summary] finished");
            })
        })?)
    }
}

/// Normalize a raw LLM response to a single lowercase English word.
/// Returns the normalized word, or an empty string when no ASCII letters remain.
fn normalize_title_word(raw: &str) -> String {
    raw.split_whitespace()
        .next()
        .map(|tok| {
            tok.chars()
                .filter(|c| c.is_ascii_alphabetic())
                .collect::<String>()
                .to_lowercase()
        })
        .unwrap_or_default()
}

/// Scan all threads and generate summaries for those that need them.
async fn generate_thread_summaries(
    conv_mgr: &ConversationManager,
    llm_backend: Option<&dyn LlmBackend>,
    language: &str,
) -> anyhow::Result<()> {
    let backend = match llm_backend {
        Some(b) => b,
        None => {
            tracing::debug!("thread_summary: no LLM available, skipping");
            return Ok(());
        }
    };

    let thread_ids = conv_mgr.list_thread_ids();

    for thread_id in &thread_ids {
        let rounds = conv_mgr.count_rounds(thread_id);
        if rounds == 0 {
            continue;
        }

        let meta = conv_mgr.load_meta(thread_id);

        // Decide whether to (re)generate summary:
        // 1. No summary yet
        // 2. Summary was generated with < 5 rounds AND there are new rounds
        let needs_summary = match (meta.summary.as_ref(), meta.summary_rounds) {
            (None, _) => true,
            (Some(_), Some(prev_rounds)) => prev_rounds < 5 && rounds > prev_rounds,
            _ => false,
        };

        let mut updated_meta = meta.clone();
        let mut meta_dirty = false;

        if needs_summary {
            // Get up to 5 exchanges for summary generation
            let exchanges = conv_mgr.get_all_exchanges(thread_id);
            let exchanges: Vec<_> = exchanges.into_iter().take(5).collect();
            if exchanges.is_empty() {
                continue;
            }

            // Build conversation text
            let mut conversation_text = String::new();
            for (user, assistant) in &exchanges {
                conversation_text.push_str(&format!("User: {}\nAssistant: {}\n\n", user, assistant));
            }

            let use_case = UseCase::Chat;
            let max_content_chars = backend.max_content_chars();
            let req = LlmRequest {
                context: format!("<conversation>\n{}</conversation>", conversation_text),
                query: Some(template::append_language_instruction(template::THREAD_SUMMARY_PROMPT, language)),
                trigger: TriggerType::AutoPattern,
                session_ids: vec![],
                use_case,
                max_content_chars,
                system_prompt: None,
                enable_thinking: Some(false),
                tools: vec![],
                extra_messages: vec![],
            };

            match backend.complete(&req).await {
                Ok(resp) => {
                    let summary = crate::strip_thinking_block(&resp.text()).trim().to_string();
                    if !summary.is_empty() {
                        updated_meta.summary = Some(summary);
                        updated_meta.summary_rounds = Some(rounds);
                        // Summary changed, invalidate the derived title_word so it
                        // gets regenerated from the new summary below.
                        updated_meta.title_word = None;
                        meta_dirty = true;
                        tracing::info!(
                            "thread_summary: generated summary for thread {} ({} rounds)",
                            thread_id,
                            rounds
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "thread_summary: LLM failed for thread {}: {}",
                        thread_id,
                        e
                    );
                }
            }
        }

        // After (re)generating summary, derive a single English word for the
        // tmux window label when the thread has a summary but no title_word.
        if updated_meta.summary.is_some() && updated_meta.title_word.is_none() {
            let summary_text = updated_meta.summary.clone().unwrap();
            let use_case = UseCase::Chat;
            let max_content_chars = backend.max_content_chars();
            // Prompt asks for a single English word; do NOT append a language instruction here.
            let req = LlmRequest {
                context: format!("<summary>\n{}\n</summary>", summary_text),
                query: Some(template::THREAD_TITLE_WORD_PROMPT.to_string()),
                trigger: TriggerType::AutoPattern,
                session_ids: vec![],
                use_case,
                max_content_chars,
                system_prompt: None,
                enable_thinking: Some(false),
                tools: vec![],
                extra_messages: vec![],
            };

            match backend.complete(&req).await {
                Ok(resp) => {
                    let raw = crate::strip_thinking_block(&resp.text());
                    let word = normalize_title_word(raw.trim());
                    if !word.is_empty() {
                        updated_meta.title_word = Some(word.clone());
                        meta_dirty = true;
                        tracing::info!(
                            "thread_summary: generated title_word '{}' for thread {}",
                            word,
                            thread_id
                        );
                    } else {
                        tracing::warn!(
                            "thread_summary: LLM did not yield an English word for thread {}: {:?}",
                            thread_id,
                            raw
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "thread_summary: title_word LLM failed for thread {}: {}",
                        thread_id,
                        e
                    );
                }
            }
        }

        if meta_dirty {
            // Issue #587: apply only the fields this task owns to a fresh
            // on-disk meta, rather than saving our stale clone. The LLM
            // calls above can take seconds, during which user commands
            // (e.g. `/thread sandbox off`) may have mutated unrelated
            // fields.
            let new_summary = updated_meta.summary.clone();
            let new_summary_rounds = updated_meta.summary_rounds;
            let new_title_word = updated_meta.title_word.clone();
            conv_mgr.update_meta(thread_id, |m| {
                m.summary = new_summary;
                m.summary_rounds = new_summary_rounds;
                m.title_word = new_title_word;
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_mgr::ThreadMeta;

    fn user_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    fn assistant_msg(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "assistant", "content": text})
    }

    #[tokio::test]
    async fn test_skip_when_no_llm() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(&id, &[user_msg("hello"), assistant_msg("hi")]);

        // No LLM → should succeed without generating summary
        generate_thread_summaries(&conv_mgr, None, "en").await.unwrap();
        let meta = conv_mgr.load_meta(&id);
        assert!(meta.summary.is_none());
    }

    #[tokio::test]
    async fn test_skip_empty_threads() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let _id = conv_mgr.create_thread(ThreadMeta::default());

        // Empty thread → should be skipped
        generate_thread_summaries(&conv_mgr, None, "en").await.unwrap();
    }

    #[test]
    fn test_needs_summary_logic() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());

        // Thread with no summary needs one
        let id = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(&id, &[user_msg("q"), assistant_msg("a")]);
        let meta = conv_mgr.load_meta(&id);
        assert!(meta.summary.is_none());
        assert_eq!(conv_mgr.count_rounds(&id), 1);

        // Thread with summary at 2 rounds, now has 3 → needs re-gen
        let meta2 = ThreadMeta {
            summary: Some("old summary".to_string()),
            summary_rounds: Some(2),
            ..Default::default()
        };
        let id2 = conv_mgr.create_thread(meta2);
        conv_mgr.append_messages(&id2, &[
            user_msg("q1"), assistant_msg("a1"),
            user_msg("q2"), assistant_msg("a2"),
            user_msg("q3"), assistant_msg("a3"),
        ]);
        let meta2 = conv_mgr.load_meta(&id2);
        let rounds = conv_mgr.count_rounds(&id2);
        assert!(meta2.summary_rounds.unwrap() < 5 && rounds > meta2.summary_rounds.unwrap());

        // Thread with summary at 5 rounds → no re-gen needed
        let meta3 = ThreadMeta {
            summary: Some("complete summary".to_string()),
            summary_rounds: Some(5),
            ..Default::default()
        };
        let id3 = conv_mgr.create_thread(meta3);
        conv_mgr.append_messages(&id3, &[
            user_msg("q1"), assistant_msg("a1"),
            user_msg("q2"), assistant_msg("a2"),
            user_msg("q3"), assistant_msg("a3"),
            user_msg("q4"), assistant_msg("a4"),
            user_msg("q5"), assistant_msg("a5"),
            user_msg("q6"), assistant_msg("a6"),
        ]);
        let meta3 = conv_mgr.load_meta(&id3);
        assert_eq!(meta3.summary_rounds, Some(5));
        // Even though rounds (6) > 5, summary_rounds is already 5 so no re-gen
    }

    #[tokio::test]
    async fn test_title_override_not_touched_by_summary_loop() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = conv_mgr.create_thread(ThreadMeta {
            title_override: Some("sticky name".to_string()),
            ..Default::default()
        });
        conv_mgr.append_messages(&id, &[user_msg("q"), assistant_msg("a")]);

        // Running with no LLM should still leave title_override intact.
        generate_thread_summaries(&conv_mgr, None, "en").await.unwrap();
        let meta = conv_mgr.load_meta(&id);
        assert_eq!(meta.title_override.as_deref(), Some("sticky name"));
    }

    #[test]
    fn test_normalize_title_word() {
        assert_eq!(normalize_title_word("deploy"), "deploy");
        assert_eq!(normalize_title_word("Deploy"), "deploy");
        assert_eq!(normalize_title_word("  deploy  "), "deploy");
        assert_eq!(normalize_title_word("deploy pipeline"), "deploy");
        assert_eq!(normalize_title_word("\"deploy\""), "deploy");
        assert_eq!(normalize_title_word("deploy."), "deploy");
        assert_eq!(normalize_title_word("部署"), "");
        assert_eq!(normalize_title_word(""), "");
        assert_eq!(normalize_title_word("123"), "");
    }
}
