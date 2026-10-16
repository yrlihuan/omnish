use crate::conversation_mgr::{ConversationManager, ThreadMeta};
use crate::task_mgr::{ScheduledTask, TaskContext};
use omnish_common::config::ConfigMap;
use omnish_llm::{backend::{LlmBackend, LlmRequest, TriggerType, UseCase}, template};
use std::sync::Arc;
use tokio_cron_scheduler::Job;

/// After every candidate model failed, wait this long (and for new rounds)
/// before trying the thread again (#698).
const RETRY_AFTER_MS: u64 = 24 * 60 * 60 * 1000;

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
        let push_registry = ctx.daemon.push_registry.clone();
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
            let conv_mgr = conv_mgr.clone();
            let push_registry = push_registry.clone();
            let llm = llm_holder.read().unwrap().clone();
            let language = daemon_config.read().unwrap().client.language.clone();
            Box::pin(async move {
                tracing::debug!("task [thread_summary] started");
                let candidates = |meta: &ThreadMeta| {
                    candidate_backends(
                        meta,
                        |name| llm.get_backend_by_name(name),
                        llm.get_backend(UseCase::Chat),
                        llm.get_backend(UseCase::Summarize),
                    )
                };
                if let Err(e) = generate_thread_summaries(&conv_mgr, &candidates, &language, &push_registry, now_ms()).await {
                    tracing::warn!("task [thread_summary] failed: {}", e);
                }
                tracing::debug!("task [thread_summary] finished");
            })
        })?)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Models to try for one thread, in order (#698): the backend of the thread's
/// last successful LLM call (its messages are known to suit it, e.g. thinking
/// signatures), else the thread's selected model, then the chat and the
/// summarize backends. Each backend appears once.
fn candidate_backends(
    meta: &ThreadMeta,
    by_name: impl Fn(&str) -> Option<Arc<dyn LlmBackend>>,
    chat: Arc<dyn LlmBackend>,
    summarize: Arc<dyn LlmBackend>,
) -> Vec<Arc<dyn LlmBackend>> {
    let thread_backend = meta.last_model.as_deref().and_then(&by_name)
        .or_else(|| meta.model.as_deref().and_then(&by_name));
    let mut out: Vec<Arc<dyn LlmBackend>> = Vec::new();
    for b in thread_backend.into_iter().chain([chat, summarize]) {
        if !out.iter().any(|o| o.name() == b.name()) {
            out.push(b);
        }
    }
    out
}

/// True while a thread whose last attempt failed on every model must be left
/// alone: until 24 hours have passed and the thread has new rounds.
fn in_failure_backoff(meta: &ThreadMeta, rounds: u32, now_ms: u64) -> bool {
    match meta.summary_failed_at {
        None => false,
        Some(at) => {
            now_ms < at.saturating_add(RETRY_AFTER_MS)
                || rounds <= meta.summary_failed_rounds.unwrap_or(0)
        }
    }
}

/// Run `req` (built per backend, as limits differ) on each backend in turn
/// until one returns a response `accept` keeps. None when all failed.
async fn complete_with_fallback(
    backends: &[Arc<dyn LlmBackend>],
    thread_id: &str,
    what: &str,
    req: impl Fn(&dyn LlmBackend) -> LlmRequest,
    accept: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    for backend in backends {
        match crate::llm_pool::complete(backend.as_ref(), &req(backend.as_ref())).await {
            Ok(resp) => {
                let raw = crate::strip_thinking_block(&resp.text());
                if let Some(v) = accept(raw.trim()) {
                    return Some(v);
                }
                tracing::warn!(
                    "thread_summary: {} from {} unusable for thread {}: {:?}",
                    what, backend.name(), thread_id, raw
                );
            }
            Err(e) => {
                tracing::warn!(
                    "thread_summary: {} LLM {} failed for thread {}: {}",
                    what, backend.name(), thread_id, e
                );
            }
        }
    }
    None
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
/// `candidates` gives the models to try for a thread (empty: no LLM).
async fn generate_thread_summaries(
    conv_mgr: &ConversationManager,
    candidates: &(dyn Fn(&ThreadMeta) -> Vec<Arc<dyn LlmBackend>> + Sync),
    language: &str,
    push_registry: &omnish_transport::rpc_server::PushRegistry,
    now_ms: u64,
) -> anyhow::Result<()> {
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
        let needs_title = meta.summary.is_some() && meta.title_word.is_none();
        if !needs_summary && !needs_title {
            continue;
        }
        if in_failure_backoff(&meta, rounds, now_ms) {
            continue;
        }
        let backends = candidates(&meta);
        if backends.is_empty() {
            tracing::debug!("thread_summary: no LLM available, skipping");
            return Ok(());
        }

        let mut updated_meta = meta.clone();
        let mut meta_dirty = false;
        let mut failed = false;

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
            let context = format!("<conversation>\n{}</conversation>", conversation_text);
            let query = template::append_language_instruction(template::THREAD_SUMMARY_PROMPT, language);

            let summary = complete_with_fallback(
                &backends,
                thread_id,
                "summary",
                |b| LlmRequest {
                    context: context.clone(),
                    query: Some(query.clone()),
                    trigger: TriggerType::AutoPattern,
                    session_ids: vec![],
                    use_case: UseCase::Chat,
                    max_content_chars: b.max_content_chars(),
                    system_prompt: None,
                    enable_thinking: Some(false),
                    tools: vec![],
                    extra_messages: vec![],
                },
                |text| (!text.is_empty()).then(|| text.to_string()),
            ).await;
            match summary {
                Some(summary) => {
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
                None => failed = true,
            }
        }

        // After (re)generating summary, derive a single English word for the
        // tmux window label when the thread has a summary but no title_word.
        if !failed && updated_meta.summary.is_some() && updated_meta.title_word.is_none() {
            let context = format!("<summary>\n{}\n</summary>", updated_meta.summary.as_deref().unwrap());
            // Prompt asks for a single English word; do NOT append a language instruction here.
            let word = complete_with_fallback(
                &backends,
                thread_id,
                "title_word",
                |b| LlmRequest {
                    context: context.clone(),
                    query: Some(template::THREAD_TITLE_WORD_PROMPT.to_string()),
                    trigger: TriggerType::AutoPattern,
                    session_ids: vec![],
                    use_case: UseCase::Chat,
                    max_content_chars: b.max_content_chars(),
                    system_prompt: None,
                    enable_thinking: Some(false),
                    tools: vec![],
                    extra_messages: vec![],
                },
                |text| Some(normalize_title_word(text)).filter(|w| !w.is_empty()),
            ).await;
            match word {
                Some(word) => {
                    updated_meta.title_word = Some(word.clone());
                    meta_dirty = true;
                    crate::push::broadcast_server_push(
                        push_registry,
                        "thread_title_update",
                        serde_json::json!({ "thread_id": thread_id, "title": word }),
                    ).await;
                    tracing::info!(
                        "thread_summary: generated title_word '{}' for thread {}",
                        word,
                        thread_id
                    );
                }
                None => failed = true,
            }
        }

        if failed {
            tracing::warn!(
                "thread_summary: all models failed for thread {}, retry after 24h once it has new messages",
                thread_id
            );
            updated_meta.summary_failed_at = Some(now_ms);
            updated_meta.summary_failed_rounds = Some(rounds);
            meta_dirty = true;
        } else if updated_meta.summary_failed_at.is_some() {
            updated_meta.summary_failed_at = None;
            updated_meta.summary_failed_rounds = None;
            meta_dirty = true;
        }

        if meta_dirty {
            // Issue #587: apply only the fields this task owns to a fresh
            // on-disk meta, rather than saving our stale clone. The LLM
            // calls above can take seconds, during which user commands
            // (e.g. `/thread sandbox off`) may have mutated unrelated
            // fields.
            conv_mgr.update_meta(thread_id, |m| {
                m.summary = updated_meta.summary;
                m.summary_rounds = updated_meta.summary_rounds;
                m.title_word = updated_meta.title_word;
                m.summary_failed_at = updated_meta.summary_failed_at;
                m.summary_failed_rounds = updated_meta.summary_failed_rounds;
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

    fn no_llm(_: &ThreadMeta) -> Vec<Arc<dyn LlmBackend>> {
        Vec::new()
    }

    /// Records every call; fails or answers with a fixed text.
    struct MockLlm {
        name: &'static str,
        reply: Option<&'static str>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl MockLlm {
        fn new(name: &'static str, reply: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self { name, reply, calls: Default::default() })
        }
        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl LlmBackend for MockLlm {
        async fn complete(&self, req: &LlmRequest) -> anyhow::Result<omnish_llm::backend::LlmResponse> {
            use omnish_llm::backend::{ContentBlock, LlmResponse, StopReason};
            self.calls.lock().unwrap().push(req.context.clone());
            match self.reply {
                Some(text) => Ok(LlmResponse {
                    content: vec![ContentBlock::Text(text.to_string())],
                    stop_reason: StopReason::EndTurn,
                    model: self.name.to_string(),
                    usage: None,
                }),
                None => anyhow::bail!("{} unavailable", self.name),
            }
        }
        fn name(&self) -> &str {
            self.name
        }
        fn model_name(&self) -> &str {
            self.name
        }
    }

    fn registry() -> omnish_transport::rpc_server::PushRegistry {
        std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[test]
    fn test_candidate_backends_order() {
        let thread = MockLlm::new("thread", None);
        let picked = MockLlm::new("picked", None);
        let chat = MockLlm::new("chat", None);
        let summarize = MockLlm::new("summarize", None);
        let by_name = |name: &str| -> Option<Arc<dyn LlmBackend>> {
            match name {
                "thread" => Some(thread.clone()),
                "picked" => Some(picked.clone()),
                "chat" => Some(chat.clone()),
                _ => None,
            }
        };
        let names = |meta: &ThreadMeta| -> Vec<String> {
            candidate_backends(meta, by_name, chat.clone(), summarize.clone())
                .iter().map(|b| b.name().to_string()).collect()
        };
        // Last successful backend first, then chat and summarize.
        let meta = ThreadMeta { last_model: Some("thread".into()), model: Some("picked".into()), ..Default::default() };
        assert_eq!(names(&meta), ["thread", "chat", "summarize"]);
        // Last backend no longer configured: the thread's selected model.
        let meta = ThreadMeta { last_model: Some("gone".into()), model: Some("picked".into()), ..Default::default() };
        assert_eq!(names(&meta), ["picked", "chat", "summarize"]);
        // Thread already on the chat backend: not tried twice.
        let meta = ThreadMeta { last_model: Some("chat".into()), ..Default::default() };
        assert_eq!(names(&meta), ["chat", "summarize"]);
        let meta = ThreadMeta::default();
        assert_eq!(names(&meta), ["chat", "summarize"]);
    }

    #[tokio::test]
    async fn test_falls_back_to_next_model() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(&id, &[user_msg("q"), assistant_msg("a")]);

        let thread = MockLlm::new("thread", None);
        let chat = MockLlm::new("chat", Some("deploy"));
        let (t, c) = (thread.clone(), chat.clone());
        let candidates = move |_: &ThreadMeta| -> Vec<Arc<dyn LlmBackend>> { vec![t.clone(), c.clone()] };
        generate_thread_summaries(&conv_mgr, &candidates, "en", &registry(), 1_000).await.unwrap();

        let meta = conv_mgr.load_meta(&id);
        assert_eq!(meta.summary.as_deref(), Some("deploy"));
        assert_eq!(meta.title_word.as_deref(), Some("deploy"));
        assert!(meta.summary_failed_at.is_none());
        // Thread model tried for both summary and title, chat answered both.
        assert_eq!(thread.calls(), 2);
        assert_eq!(chat.calls(), 2);
    }

    #[tokio::test]
    async fn test_backoff_after_all_models_fail() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(&id, &[user_msg("q"), assistant_msg("a")]);

        let a = MockLlm::new("a", None);
        let b = MockLlm::new("b", None);
        let (ac, bc) = (a.clone(), b.clone());
        let failing = move |_: &ThreadMeta| -> Vec<Arc<dyn LlmBackend>> { vec![ac.clone(), bc.clone()] };
        let t0 = 1_000_000;
        generate_thread_summaries(&conv_mgr, &failing, "en", &registry(), t0).await.unwrap();
        assert_eq!((a.calls(), b.calls()), (1, 1), "each model tried once");
        let meta = conv_mgr.load_meta(&id);
        assert!(meta.summary.is_none());
        assert_eq!(meta.summary_failed_at, Some(t0));
        assert_eq!(meta.summary_failed_rounds, Some(1));

        // Next ticks: no LLM call, neither soon after nor after 24h without
        // new messages.
        generate_thread_summaries(&conv_mgr, &failing, "en", &registry(), t0 + 60_000).await.unwrap();
        generate_thread_summaries(&conv_mgr, &failing, "en", &registry(), t0 + RETRY_AFTER_MS + 1).await.unwrap();
        assert_eq!((a.calls(), b.calls()), (1, 1));

        // New messages but within 24h: still waiting.
        conv_mgr.append_messages(&id, &[user_msg("q2"), assistant_msg("a2")]);
        generate_thread_summaries(&conv_mgr, &failing, "en", &registry(), t0 + 60_000).await.unwrap();
        assert_eq!((a.calls(), b.calls()), (1, 1));

        // 24h passed and new messages: retried, success clears the marker.
        let ok = MockLlm::new("ok", Some("deploy"));
        let okc = ok.clone();
        let working = move |_: &ThreadMeta| -> Vec<Arc<dyn LlmBackend>> { vec![okc.clone()] };
        generate_thread_summaries(&conv_mgr, &working, "en", &registry(), t0 + RETRY_AFTER_MS + 1).await.unwrap();
        assert_eq!(ok.calls(), 2);
        let meta = conv_mgr.load_meta(&id);
        assert_eq!(meta.summary.as_deref(), Some("deploy"));
        assert_eq!(meta.summary_rounds, Some(2));
        assert!(meta.summary_failed_at.is_none());
        assert!(meta.summary_failed_rounds.is_none());
    }

    #[tokio::test]
    async fn test_unusable_title_word_counts_as_failure() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(&id, &[user_msg("q"), assistant_msg("a")]);

        // Summary works but no model yields an English word.
        let cjk = MockLlm::new("cjk", Some("部署"));
        let c = cjk.clone();
        let candidates = move |_: &ThreadMeta| -> Vec<Arc<dyn LlmBackend>> { vec![c.clone()] };
        generate_thread_summaries(&conv_mgr, &candidates, "en", &registry(), 5).await.unwrap();
        let meta = conv_mgr.load_meta(&id);
        assert_eq!(meta.summary.as_deref(), Some("部署"));
        assert!(meta.title_word.is_none());
        assert_eq!(meta.summary_failed_at, Some(5));

        // Not retried every minute.
        generate_thread_summaries(&conv_mgr, &candidates, "en", &registry(), 60_005).await.unwrap();
        assert_eq!(cjk.calls(), 2);
    }

    #[tokio::test]
    async fn test_skip_when_no_llm() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let id = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(&id, &[user_msg("hello"), assistant_msg("hi")]);

        // No LLM → should succeed without generating summary
        let registry = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        generate_thread_summaries(&conv_mgr, &no_llm, "en", &registry, 0).await.unwrap();
        let meta = conv_mgr.load_meta(&id);
        assert!(meta.summary.is_none());
    }

    #[tokio::test]
    async fn test_skip_empty_threads() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().to_path_buf());
        let _id = conv_mgr.create_thread(ThreadMeta::default());

        // Empty thread → should be skipped
        let registry = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        generate_thread_summaries(&conv_mgr, &no_llm, "en", &registry, 0).await.unwrap();
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
        let registry = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        generate_thread_summaries(&conv_mgr, &no_llm, "en", &registry, 0).await.unwrap();
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
