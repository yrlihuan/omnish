use anyhow::Result;
use omnish_daemon::conversation_mgr::ConversationManager;
use omnish_daemon::session_mgr::SessionManager;
use omnish_daemon::task_mgr::TaskManager;
use omnish_llm::backend::{LlmBackend, LlmRequest, TriggerType, UseCase};
use omnish_protocol::message::*;
use omnish_transport::rpc_server::RpcServer;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_rustls::TlsAcceptor;

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    task_mgr: Arc<Mutex<TaskManager>>,
    conv_mgr: Arc<ConversationManager>,
}

impl DaemonServer {
    pub fn new(
        session_mgr: Arc<SessionManager>,
        llm_backend: Option<Arc<dyn LlmBackend>>,
        task_mgr: Arc<Mutex<TaskManager>>,
        conv_mgr: Arc<ConversationManager>,
    ) -> Self {
        Self { session_mgr, llm_backend, task_mgr, conv_mgr }
    }

    pub async fn run(
        &self,
        addr: &str,
        auth_token: String,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Result<()> {
        let mut server = RpcServer::bind(addr).await?;
        tracing::info!("omnishd listening on {}", addr);

        let mgr = self.session_mgr.clone();
        let llm = self.llm_backend.clone();
        let task_mgr = self.task_mgr.clone();
        let conv_mgr = self.conv_mgr.clone();

        server
            .serve(
                move |msg| {
                    let mgr = mgr.clone();
                    let llm = llm.clone();
                    let task_mgr = task_mgr.clone();
                    let conv_mgr = conv_mgr.clone();
                    Box::pin(async move { handle_message(msg, mgr, &llm, &task_mgr, &conv_mgr).await })
                },
                Some(auth_token),
                tls_acceptor,
            )
            .await
    }
}

async fn handle_message(
    msg: Message,
    mgr: Arc<SessionManager>,
    llm: &Option<Arc<dyn LlmBackend>>,
    task_mgr: &Arc<Mutex<TaskManager>>,
    conv_mgr: &Arc<ConversationManager>,
) -> Message {
    // Shadow with reference for existing code; use mgr_arc for spawned tasks
    let mgr_arc = mgr;
    let mgr = &*mgr_arc;
    match msg {
        Message::SessionStart(s) => {
            if let Err(e) = mgr
                .register(&s.session_id, s.parent_session_id, s.attrs)
                .await
            {
                tracing::error!("register error: {}", e);
            }
            Message::Ack
        }
        Message::SessionEnd(s) => {
            if let Err(e) = mgr.end_session(&s.session_id).await {
                tracing::error!("end_session error: {}", e);
            }
            Message::Ack
        }
        Message::SessionUpdate(su) => {
            if let Err(e) = mgr.update_attrs(&su.session_id, su.timestamp_ms, su.attrs).await {
                tracing::error!("update_attrs error: {}", e);
            }
            Message::Ack
        }
        Message::IoData(io) => {
            let dir = match io.direction {
                IoDirection::Input => 0,
                IoDirection::Output => 1,
            };
            if let Err(e) = mgr
                .write_io(&io.session_id, io.timestamp_ms, dir, &io.data)
                .await
            {
                tracing::error!("write_io error: {}", e);
            }
            Message::Ack
        }
        Message::CommandComplete(cc) => {
            if let Err(e) = mgr.receive_command(&cc.session_id, cc.record).await {
                tracing::error!("receive_command error: {}", e);
            }
            // Proactively warm the LLM KV cache if the context prefix changed
            if llm.is_some() {
                let mgr = mgr_arc.clone();
                let llm = llm.clone();
                let sid = cc.session_id.clone();
                tokio::spawn(async move {
                    try_warmup_kv_cache(&sid, &mgr, &llm).await;
                });
            }
            Message::Ack
        }
        Message::Request(req) => {
            if req.query.starts_with("__cmd:") {
                let content = handle_builtin_command(&req, &mgr, &task_mgr, &llm).await;
                return Message::Response(Response {
                    request_id: req.request_id,
                    content,
                    is_streaming: false,
                    is_final: true,
                });
            }

            let content = if let Some(ref backend) = llm {
                match handle_llm_request(&req, mgr, backend).await {
                    Ok(response) => response.content,
                    Err(e) => {
                        tracing::error!("LLM request failed: {}", e);
                        format!("Error: {}", e)
                    }
                }
            } else {
                "(LLM backend not configured)".to_string()
            };

            Message::Response(Response {
                request_id: req.request_id,
                content,
                is_streaming: false,
                is_final: true,
            })
        }
        Message::CompletionRequest(req) => {
            tracing::debug!(
                "CompletionRequest: input={:?} seq={}",
                req.input,
                req.sequence_id
            );
            if let Some(ref backend) = llm {
                match handle_completion_request(&req, mgr, backend).await {
                    Ok(suggestions) => {
                        Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
                            sequence_id: req.sequence_id,
                            suggestions,
                        })
                    }
                    Err(e) => {
                        tracing::error!("Completion request failed: {}", e);
                        Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
                            sequence_id: req.sequence_id,
                            suggestions: vec![],
                        })
                    }
                }
            } else {
                Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
                    sequence_id: req.sequence_id,
                    suggestions: vec![],
                })
            }
        }
        Message::CompletionSummary(summary) => {
            // Update pending sample's accepted flag (issue #101)
            mgr.update_pending_sample_accepted(&summary.session_id, summary.accepted).await;

            if let Err(e) = mgr.receive_completion(summary.clone()).await {
                tracing::error!("receive_completion error: {}", e);
            }
            tracing::debug!(
                "CompletionSummary: session={} seq={} accepted={} latency_ms={} dwell_time_ms={:?}",
                summary.session_id,
                summary.sequence_id,
                summary.accepted,
                summary.latency_ms,
                summary.dwell_time_ms
            );
            Message::Ack
        }
        Message::ChatStart(cs) => {
            let thread_id = if cs.new_thread {
                conv_mgr.create_thread()
            } else {
                conv_mgr.get_latest_thread().unwrap_or_else(|| conv_mgr.create_thread())
            };
            let (last_exchange, earlier_count) = conv_mgr.get_last_exchange(&thread_id);
            Message::ChatReady(ChatReady {
                request_id: cs.request_id,
                thread_id,
                last_exchange,
                earlier_count,
            })
        }
        Message::ChatMessage(cm) => {
            let content = if let Some(ref backend) = llm {
                let conversation = conv_mgr.load_messages(&cm.thread_id);
                let use_case = UseCase::Chat;
                let max_context_chars = backend.max_content_chars_for_use_case(use_case);

                // Get terminal context only for the first message in a thread
                let context = if conversation.is_empty() {
                    let dummy_req = Request {
                        request_id: cm.request_id.clone(),
                        session_id: cm.session_id.clone(),
                        query: String::new(),
                        scope: RequestScope::AllSessions,
                    };
                    resolve_chat_context(&dummy_req, mgr, max_context_chars).await.unwrap_or_default()
                } else {
                    String::new()
                };

                let llm_req = LlmRequest {
                    context,
                    query: Some(cm.query.clone()),
                    trigger: TriggerType::Manual,
                    session_ids: vec![cm.session_id.clone()],
                    use_case,
                    max_content_chars: max_context_chars,
                    conversation,
                };

                let start = std::time::Instant::now();
                match backend.complete(&llm_req).await {
                    Ok(response) => {
                        tracing::info!("Chat LLM completed in {:?} (thread={})", start.elapsed(), cm.thread_id);
                        conv_mgr.append_exchange(&cm.thread_id, &cm.query, &response.content);
                        response.content
                    }
                    Err(e) => {
                        tracing::error!("Chat LLM failed: {}", e);
                        format!("Error: {}", e)
                    }
                }
            } else {
                "(LLM backend not configured)".to_string()
            };

            Message::ChatResponse(ChatResponse {
                request_id: cm.request_id,
                thread_id: cm.thread_id,
                content,
            })
        }
        _ => Message::Ack,
    }
}

async fn try_warmup_kv_cache(
    session_id: &str,
    mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
) {
    let backend = match llm {
        Some(b) => b,
        None => return,
    };

    let max_chars = backend.max_content_chars_for_use_case(UseCase::Completion);

    let new_context = match mgr.check_and_warmup_context(session_id, max_chars).await {
        Ok(Some(ctx)) => ctx,
        Ok(None) => return, // prefix stable, no warmup needed
        Err(e) => {
            tracing::debug!("KV cache warmup context check failed: {}", e);
            return;
        }
    };

    let prompt = omnish_llm::template::build_simple_completion_content(&new_context, "", 0);
    let req = LlmRequest {
        context: String::new(),
        query: Some(prompt),
        trigger: TriggerType::Manual,
        session_ids: vec![session_id.to_string()],
        use_case: UseCase::Completion,
        max_content_chars: max_chars,
        conversation: vec![],
    };

    match backend.complete(&req).await {
        Ok(_) => tracing::debug!("KV cache warmup completed for session {}", session_id),
        Err(e) => tracing::debug!("KV cache warmup failed for session {}: {}", session_id, e),
    }
}

/// Resolve context for chat requests (without history, only recent commands with output).
/// This is used for LLM chat/analysis requests where we only want recent commands.
async fn resolve_chat_context(req: &Request, mgr: &SessionManager, max_context_chars: Option<usize>) -> Result<String> {
    match &req.scope {
        RequestScope::CurrentSession => mgr.get_chat_context(&req.session_id, max_context_chars).await,
        RequestScope::AllSessions => mgr.get_all_sessions_chat_context(&req.session_id, max_context_chars).await,
        RequestScope::Sessions(ids) => {
            let mut combined = String::new();
            for sid in ids {
                match mgr.get_chat_context(sid, max_context_chars).await {
                    Ok(ctx) => {
                        combined.push_str(&format!("\n=== Session {} ===\n", sid));
                        combined.push_str(&ctx);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get chat context for session {}: {}", sid, e);
                    }
                }
            }
            Ok(combined)
        }
    }
}


async fn handle_builtin_command(req: &Request, mgr: &SessionManager, task_mgr: &Mutex<TaskManager>, llm_backend: &Option<Arc<dyn LlmBackend>>) -> String {
    let sub = req.query.strip_prefix("__cmd:").unwrap_or("");
    // Handle /context <scenario> for showing context for different scenarios
    if let Some(scenario) = sub.strip_prefix("context ") {
        return handle_context_scenario(scenario, req, mgr, llm_backend).await;
    }
    match sub {
        "context" => {
            // Default to completion context (most common LLM use case)
            match mgr.build_completion_context(&req.session_id, None).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        "sessions" => mgr.format_sessions_list(&req.session_id).await,
        "session" => match get_session_debug_info(&req.session_id, mgr).await {
            Ok(info) => info,
            Err(e) => format!("Error: {}", e),
        },
        sub if sub == "tasks" || sub.starts_with("tasks ") => {
            handle_tasks_command(sub, task_mgr).await
        }
        other => format!("Unknown command: {}", other),
    }
}

/// Handle /context <scenario> to show context for different use cases.
async fn handle_context_scenario(scenario: &str, req: &Request, mgr: &SessionManager, llm_backend: &Option<Arc<dyn LlmBackend>>) -> String {
    match scenario {
        "chat" | "analysis" => {
            // Chat/analysis context - only recent commands with output (no history)
            match resolve_chat_context(req, mgr, None).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        "auto-complete" | "completion" => {
            // Auto-complete context - uses CompletionFormatter with elastic window
            let max_chars = llm_backend
                .as_ref()
                .and_then(|b| b.max_content_chars_for_use_case(UseCase::Completion));
            match mgr.build_completion_context(&req.session_id, max_chars).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        "daily-notes" => {
            // Show the same context that gets sent to the LLM for daily notes:
            // command table only
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let since_ms = now_ms.saturating_sub(24 * 3600 * 1000);
            let commands = mgr.collect_recent_commands(since_ms).await;
            if commands.is_empty() {
                return "No commands in the past 24 hours".to_string();
            }
            omnish_daemon::daily_notes::build_daily_notes_context(&commands)
        }
        "hourly-notes" | "hourly" => {
            // Show commands from past hour using build_hourly_summary_context
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let since_ms = now_ms.saturating_sub(3600 * 1000);
            let commands = mgr.collect_recent_commands(since_ms).await;
            if commands.is_empty() {
                return "No commands in the past hour".to_string();
            }
            let max_chars = llm_backend
                .as_ref()
                .and_then(|b| b.max_content_chars());
            let config = mgr.get_hourly_summary_config();
            match mgr.build_hourly_summary_context(&commands, max_chars, &config).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        _ => format!("Unknown scenario: {}. Available: chat, auto-complete, daily-notes, hourly-notes", scenario),
    }
}

async fn handle_tasks_command(
    sub: &str,
    task_mgr: &Mutex<TaskManager>,
) -> String {
    let parts: Vec<&str> = sub.split_whitespace().collect();
    match parts.as_slice() {
        ["tasks"] => {
            let mgr = task_mgr.lock().await;
            mgr.format_list()
        }
        ["tasks", "disable", name] => {
            let mut mgr = task_mgr.lock().await;
            match mgr.disable(name).await {
                Ok(()) => format!("Disabled task '{}'", name),
                Err(e) => format!("Error: {}", e),
            }
        }
        _ => "Usage: tasks [disable <name>]".to_string(),
    }
}

async fn get_session_debug_info(session_id: &str, mgr: &SessionManager) -> Result<String> {
    let (meta, cmd_count, last_active_duration, last_update) = mgr.get_session_debug_info(session_id).await?;
    let commands = mgr.get_commands(session_id).await?;

    let mut info = String::new();
    info.push_str(&format!("Session ID: {}\n", session_id));
    info.push_str(&format!("Started at: {}\n", meta.started_at));
    if let Some(ended_at) = &meta.ended_at {
        info.push_str(&format!("Ended at: {}\n", ended_at));
    } else {
        info.push_str(&format!("Status: Active\n"));
        info.push_str(&format!("Last active: {}s ago\n", last_active_duration.as_secs()));
    }

    // Display last update time from SessionUpdate
    if let Some(ts) = last_update {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let secs_ago = now_ms.saturating_sub(ts) / 1000;
        info.push_str(&format!("Last update: {}s ago\n", secs_ago));
    } else {
        info.push_str("Last update: never\n");
    }

    info.push_str(&format!("Commands recorded: {}\n", cmd_count));

    // Session attributes (probes)
    info.push_str("\nSession attributes:\n");
    let mut attrs: Vec<_> = meta.attrs.iter().collect();
    attrs.sort_by_key(|(k, _)| *k);
    for (key, value) in attrs {
        info.push_str(&format!("  {}: {}\n", key, value));
    }

    // Command statistics
    if !commands.is_empty() {
        let meaningful_commands: Vec<_> = commands.iter()
            .filter(|c| c.command_line.is_some())
            .collect();

        info.push_str(&format!("\nCommand statistics:\n"));
        info.push_str(&format!("  Total commands: {}\n", commands.len()));
        info.push_str(&format!("  Meaningful commands: {}\n", meaningful_commands.len()));

        if let (Some(first), Some(last)) = (meaningful_commands.first(), meaningful_commands.last()) {
            info.push_str(&format!("  First command: {} (at {})\n",
                first.command_line.as_deref().unwrap_or("unknown"),
                first.started_at));
            info.push_str(&format!("  Last command: {} (at {})\n",
                last.command_line.as_deref().unwrap_or("unknown"),
                last.started_at));
        }
    }

    Ok(info)
}

async fn handle_llm_request(
    req: &Request,
    mgr: &SessionManager,
    backend: &Arc<dyn LlmBackend>,
) -> Result<omnish_llm::backend::LlmResponse> {
    let use_case = UseCase::Chat;
    let max_context_chars = backend.max_content_chars_for_use_case(use_case);
    let context = resolve_chat_context(req, mgr, max_context_chars).await?;

    let llm_req = LlmRequest {
        context,
        query: Some(req.query.clone()),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        conversation: vec![],
    };

    let start = std::time::Instant::now();
    let result = backend.complete(&llm_req).await;
    let duration = start.elapsed();

    match &result {
        Ok(response) => {
            tracing::info!(
                "LLM request completed in {:?} (session={}, model={}, type=manual)",
                duration,
                req.session_id,
                response.model
            );

            // Log thinking content if present
            if let Some(ref thinking) = response.thinking {
                tracing::debug!("LLM thinking: {}", thinking);
            }
        }
        Err(e) => {
            tracing::warn!(
                "LLM request failed after {:?} (session={}, error={})",
                duration,
                req.session_id,
                e
            );
        }
    }

    result
}

async fn handle_completion_request(
    req: &omnish_protocol::message::CompletionRequest,
    mgr: &SessionManager,
    backend: &Arc<dyn LlmBackend>,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let use_case = UseCase::Completion;
    let max_context_chars = backend.max_content_chars_for_use_case(use_case);

    // Get previous context for prefix match ratio calculation
    let last_context = mgr.get_last_completion_context().await;

    let context = mgr.build_completion_context(&req.session_id, max_context_chars).await?;

    // Log prefix match ratio with previous completion request
    if !last_context.is_empty() {
        let common_prefix_len = last_context
            .bytes()
            .zip(context.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let ratio = common_prefix_len as f64 / last_context.len() as f64;
        tracing::debug!(
            "Completion context prefix ratio: {:.3} (common={}/old={}, session={}, seq={})",
            ratio,
            common_prefix_len,
            last_context.len(),
            req.session_id,
            req.sequence_id
        );
    }

    let prompt =
        omnish_llm::template::build_simple_completion_content(&context, &req.input, req.cursor_pos);
    let prompt_clone = prompt.clone();

    let llm_req = LlmRequest {
        context: String::new(),
        query: Some(prompt),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        conversation: vec![],
    };

    let start = std::time::Instant::now();
    let result = backend.complete(&llm_req).await;
    let duration = start.elapsed();

    // Format duration: use %.3f when > 1s, otherwise show as milliseconds
    let duration_secs = duration.as_secs_f64();
    let duration_str = if duration_secs > 1.0 {
        format!("{:.3}s", duration_secs)
    } else {
        format!("{}ms", duration.as_millis())
    };

    match &result {
        Ok(response) => {
            if duration_secs > 1.5 {
                // Slow requests (>1.5s) logged as WARN so tracing colors them
                tracing::warn!(
                    "Completion LLM request completed in {} (session={}, model={}, sequence_id={}, input_len={})",
                    duration_str, req.session_id, response.model, req.sequence_id, req.input.len()
                );
            } else {
                tracing::info!(
                    "Completion LLM request completed in {} (session={}, model={}, sequence_id={}, input_len={})",
                    duration_str, req.session_id, response.model, req.sequence_id, req.input.len()
                );
            }
            tracing::debug!("Completion LLM raw response: {:?}", response.content);

            // Log thinking content if present
            if let Some(ref thinking) = response.thinking {
                tracing::debug!("Completion LLM thinking: {}", thinking);
            }
        }
        Err(e) => {
            tracing::warn!(
                "Completion LLM request failed after {} (session={}, sequence_id={}, input_len={}, error={})",
                duration_str, req.session_id, req.sequence_id, req.input.len(), e
            );
        }
    }

    let response = result?;
    let suggestions = parse_completion_suggestions(&response.content)?;

    // Truncate suggestions at && when user input doesn't contain && (issue #107)
    let suggestions = if req.input.contains("&&") {
        suggestions
    } else {
        let mut seen = std::collections::HashSet::new();
        suggestions
            .into_iter()
            .map(|mut s| {
                if let Some(pos) = s.text.find("&&") {
                    s.text = s.text[..pos].trim_end().to_string();
                }
                s
            })
            .filter(|s| !s.text.is_empty() && seen.insert(s.text.clone()))
            .collect()
    };

    // Store pending sample for completion sampling (issue #101)
    let suggestion_texts: Vec<String> = suggestions.iter().map(|s| s.text.clone()).collect();
    mgr.store_pending_sample(omnish_store::sample::PendingSample {
        session_id: req.session_id.clone(),
        context,
        prompt: prompt_clone,
        suggestions: suggestion_texts,
        input: req.input.clone(),
        cwd: req.cwd.clone(),
        latency_ms: duration.as_millis() as u64,
        accepted: false,
        created_at: std::time::Instant::now(),
    }).await;

    Ok(suggestions)
}

fn parse_completion_suggestions(
    content: &str,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let trimmed = content.trim();

    // Extract JSON array from response (may have surrounding text)
    let start = trimmed.find('[').unwrap_or(0);
    let end = trimmed.rfind(']').map(|i| i + 1).unwrap_or(trimmed.len());
    let json_str = &trimmed[start..end];

    // Try string array format: ["suggestion1", "suggestion2"]
    if let Ok(strings) = serde_json::from_str::<Vec<String>>(json_str) {
        let suggestions: Vec<_> = strings
            .into_iter()
            .filter(|s| !s.is_empty())
            .enumerate()
            .map(|(i, text)| omnish_protocol::message::CompletionSuggestion {
                text,
                confidence: if i == 0 { 1.0 } else { 0.8 },
            })
            .collect();
        return Ok(suggestions);
    }

    // Try object array format: [{"text": "...", "confidence": 0.9}]
    #[derive(serde::Deserialize)]
    struct RawSuggestion {
        text: String,
        confidence: f32,
    }

    if let Ok(raw) = serde_json::from_str::<Vec<RawSuggestion>>(json_str) {
        return Ok(raw
            .into_iter()
            .map(|r| omnish_protocol::message::CompletionSuggestion {
                text: r.text,
                confidence: r.confidence.clamp(0.0, 1.0),
            })
            .collect());
    }

    // Fallback: treat as plain text
    let text = trimmed.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
    if text.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![omnish_protocol::message::CompletionSuggestion {
            text: text.to_string(),
            confidence: 1.0,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use omnish_llm::backend::{LlmBackend, LlmRequest, LlmResponse};
    use omnish_protocol::message::CompletionRequest as ProtoCompletionRequest;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::time::{sleep, Duration};

    // Mock LLM backend that simulates network delay
    struct MockDelayedBackend {
        delay_ms: u64,
    }

    impl MockDelayedBackend {
        fn new(delay_ms: u64) -> Self {
            Self { delay_ms }
        }
    }

    #[async_trait]
    impl LlmBackend for MockDelayedBackend {
        async fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse> {
            // Simulate network/processing delay
            sleep(Duration::from_millis(self.delay_ms)).await;
            Ok(LlmResponse {
                content: r#"[{"text": " status", "confidence": 0.9}]"#.to_string(),
                model: "mock".to_string(),
                thinking: None,
            })
        }

        fn name(&self) -> &str {
            "mock_delayed"
        }
    }

    #[test]
    fn test_parse_completion_suggestions_string_array() {
        let input = r#"["git status", "git stash"]"#;
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "git status");
        assert_eq!(result[1].text, "git stash");
    }

    #[test]
    fn test_parse_completion_suggestions_empty() {
        let result = parse_completion_suggestions("[]").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_plaintext_fallback() {
        let result = parse_completion_suggestions("status").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "status");
    }

    #[test]
    fn test_parse_completion_suggestions_empty_input() {
        assert!(parse_completion_suggestions("").unwrap().is_empty());
        assert!(parse_completion_suggestions("   ").unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_concurrent_completion_requests() {
        // Create a real SessionManager with temp directory
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(
            dir.path().to_path_buf(),
            Default::default(),
        ));

        // Register a session to have some context
        mgr.register("test_session", None, std::collections::HashMap::new())
            .await
            .unwrap();

        // Create mock backend with 100ms delay
        let backend: Arc<dyn LlmBackend> = Arc::new(MockDelayedBackend::new(100));

        // Prepare two completion requests (different sequence IDs)
        let req1 = ProtoCompletionRequest {
            session_id: "test_session".to_string(),
            input: "git".to_string(),
            cursor_pos: 3,
            sequence_id: 1,
            cwd: None,
        };

        let req2 = ProtoCompletionRequest {
            session_id: "test_session".to_string(),
            input: "ls".to_string(),
            cursor_pos: 2,
            sequence_id: 2,
            cwd: None,
        };

        // Spawn both requests concurrently
        let start = Instant::now();

        let mgr1 = mgr.clone();
        let backend1 = backend.clone();
        let handle1 =
            tokio::spawn(async move { handle_completion_request(&req1, &mgr1, &backend1).await });

        let mgr2 = mgr.clone();
        let backend2 = backend.clone();
        let handle2 =
            tokio::spawn(async move { handle_completion_request(&req2, &mgr2, &backend2).await });

        // Wait for both requests to complete
        let result1 = handle1.await.expect("Task 1 panicked");
        let result2 = handle2.await.expect("Task 2 panicked");

        let total_duration = start.elapsed();

        // Verify both requests succeeded
        assert!(result1.is_ok(), "Request 1 failed: {:?}", result1.err());
        assert!(result2.is_ok(), "Request 2 failed: {:?}", result2.err());

        // Verify we got suggestions
        let suggestions1 = result1.unwrap();
        let suggestions2 = result2.unwrap();
        assert!(!suggestions1.is_empty());
        assert!(!suggestions2.is_empty());

        // Check if requests executed concurrently
        // Sequential execution would take ~200ms (100ms each)
        // Concurrent execution should take ~100ms (overlapping delays)
        // Add some tolerance: should be less than 150ms
        println!(
            "Total duration for two 100ms requests: {:?}",
            total_duration
        );
        assert!(
            total_duration < Duration::from_millis(150),
            "Requests appear to be sequential: took {:?}, expected <150ms for concurrent execution",
            total_duration
        );
    }

    #[tokio::test]
    async fn test_concurrent_completion_requests_different_sessions() {
        // Test with different sessions to ensure per-session locking doesn't block
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(
            dir.path().to_path_buf(),
            Default::default(),
        ));

        // Register two different sessions
        mgr.register("session_a", None, std::collections::HashMap::new())
            .await
            .unwrap();
        mgr.register("session_b", None, std::collections::HashMap::new())
            .await
            .unwrap();

        // Create mock backend with 100ms delay
        let backend: Arc<dyn LlmBackend> = Arc::new(MockDelayedBackend::new(100));

        // Prepare requests for different sessions
        let req1 = ProtoCompletionRequest {
            session_id: "session_a".to_string(),
            input: "git".to_string(),
            cursor_pos: 3,
            sequence_id: 1,
            cwd: None,
        };

        let req2 = ProtoCompletionRequest {
            session_id: "session_b".to_string(),
            input: "ls".to_string(),
            cursor_pos: 2,
            sequence_id: 2,
            cwd: None,
        };

        // Spawn both requests concurrently
        let start = Instant::now();

        let mgr1 = mgr.clone();
        let backend1 = backend.clone();
        let handle1 =
            tokio::spawn(async move { handle_completion_request(&req1, &mgr1, &backend1).await });

        let mgr2 = mgr.clone();
        let backend2 = backend.clone();
        let handle2 =
            tokio::spawn(async move { handle_completion_request(&req2, &mgr2, &backend2).await });

        // Wait for both requests to complete
        let result1 = handle1.await.expect("Task 1 panicked");
        let result2 = handle2.await.expect("Task 2 panicked");

        let total_duration = start.elapsed();

        // Verify both requests succeeded
        assert!(result1.is_ok(), "Request 1 failed: {:?}", result1.err());
        assert!(result2.is_ok(), "Request 2 failed: {:?}", result2.err());

        // Check concurrency for different sessions
        println!("Total duration for two sessions: {:?}", total_duration);
        assert!(
            total_duration < Duration::from_millis(150),
            "Requests for different sessions appear to be sequential: took {:?}, expected <150ms",
            total_duration
        );
    }
}
