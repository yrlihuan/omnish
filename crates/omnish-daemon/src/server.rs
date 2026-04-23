use anyhow::Result;
use omnish_daemon::conversation_mgr::{ConversationManager, ThreadMeta};
use omnish_daemon::plugin::{PluginManager, PluginType};
use omnish_daemon::session_mgr::SessionManager;
use omnish_daemon::task_mgr::TaskManager;
use omnish_llm::backend::{ContentBlock, LlmBackend, LlmRequest, StopReason, TriggerType, UseCase};
use omnish_llm::factory::{MultiBackend, SharedLlmBackend};
use omnish_protocol::message::*;
use omnish_transport::rpc_server::{OnPushConnect, PushRegistry, RpcServer};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsAcceptor;

/// Load chat system prompt: base from embedded JSON, with optional user overrides
/// from ~/.omnish/prompts/chat.override.json (same fragment format - matching names replace).
fn load_chat_prompt() -> omnish_llm::prompt::PromptManager {
    let base = omnish_llm::prompt::PromptManager::default_chat();
    let override_path = omnish_common::config::omnish_dir().join("prompts/chat.override.json");
    match std::fs::read_to_string(&override_path) {
        Ok(content) => match omnish_llm::prompt::PromptManager::from_json(&content) {
            Ok(overrides) => {
                tracing::info!("Loaded chat prompt overrides from {}", override_path.display());
                base.merge(overrides)
            }
            Err(e) => {
                tracing::warn!("Malformed {}: {}", override_path.display(), e);
                base
            }
        },
        Err(_) => base,
    }
}

/// Strip a leading `<thinking>…</thinking>` or `<think>…</think>` block,
/// keeping the inner content visible.  Only matches when the tag appears
/// at the very start of the text (after whitespace) to avoid false positives.
fn unwrap_thinking_tags(text: &str) -> String {
    let trimmed = text.trim_start();
    for (open, close) in [("<thinking>", "</thinking>"), ("<think>", "</think>")] {
        if let Some(rest) = trimmed.strip_prefix(open) {
            return if let Some(end) = rest.find(close) {
                let inner = rest[..end].trim();
                let after = &rest[end + close.len()..];
                format!("{inner}{after}").trim().to_string()
            } else {
                rest.trim().to_string()
            };
        }
    }
    text.to_string()
}

/// Convert a leading `<thinking>…</thinking>` or `<think>…</think>` block
/// into a `# Thinking` markdown section.  Only matches the tag at the very
/// start of the text to avoid false positives.
fn thinking_to_markdown(text: &str) -> String {
    let trimmed = text.trim_start();
    for (open, close) in [("<thinking>", "</thinking>"), ("<think>", "</think>")] {
        if let Some(rest) = trimmed.strip_prefix(open) {
            if let Some(end) = rest.find(close) {
                let inner = rest[..end].trim();
                let after = rest[end + close.len()..].trim_start();
                if inner.is_empty() {
                    return after.to_string();
                }
                let mut out = format!("# Thinking\n{inner}");
                if !after.is_empty() {
                    out.push_str("\n\n# Response\n");
                    out.push_str(after);
                }
                return out.trim().to_string();
            } else {
                // Unclosed tag
                let inner = rest.trim();
                if inner.is_empty() {
                    return String::new();
                }
                return format!("# Thinking\n{inner}").trim().to_string();
            }
        }
    }
    text.to_string()
}

/// Cached state for a paused agent loop awaiting a client-side tool result.
struct AgentLoopState {
    llm_req: LlmRequest,
    /// Index up to which extra_messages have been persisted to disk.
    /// Initially equals the number of prior messages; advanced after each intermediate persist
    /// (e.g. when pausing for client-side tool execution).
    saved_up_to: usize,
    pending_tool_calls: Vec<omnish_llm::tool::ToolCall>,
    completed_results: Vec<omnish_llm::tool::ToolResult>,
    iteration: usize,
    cm: ChatMessage,
    start: std::time::Instant,
    command_query_tool: omnish_daemon::tools::command_query::CommandQueryTool,
    /// The resolved backend for this agent loop (preserves per-thread model override).
    effective_backend: Arc<dyn LlmBackend>,
    /// Number of LLM connection retries used in this agent loop.
    llm_retries: u32,
    /// Cumulative token usage across all LLM calls in this agent loop.
    cumulative_usage: omnish_llm::backend::Usage,
    /// Token usage from the most recent single LLM API call.
    last_response_usage: omnish_llm::backend::Usage,
    /// Model name from the last LLM response.
    last_model: String,
}

/// Tracks which session is actively using each thread.
struct ThreadClaim {
    session_id: String,
    last_active: std::time::Instant,
    /// Meta to save when the first ChatMessage arrives (deferred from ChatStart
    /// so that cancelling the resume doesn't overwrite the thread's stored host/cwd).
    pending_meta: Option<ThreadMeta>,
}

type ActiveThreads = Arc<Mutex<HashMap<String, ThreadClaim>>>;

/// Try to claim a thread for a session.  Returns `Ok(())` if the thread is
/// free or already owned by the same session.  Returns `Err(owner_session_id)`
/// if another session holds it.
/// On success the session's previous thread (if any) is released.
async fn try_claim_thread(active_threads: &ActiveThreads, thread_id: &str, session_id: &str) -> Result<(), String> {
    let mut threads = active_threads.lock().await;
    if let Some(claim) = threads.get(thread_id) {
        if claim.session_id != session_id {
            return Err(claim.session_id.clone());
        }
    }
    // Release any thread this session previously held, then claim the new one
    threads.retain(|_, c| c.session_id != session_id);
    threads.insert(thread_id.to_string(), ThreadClaim {
        session_id: session_id.to_string(),
        last_active: std::time::Instant::now(),
        pending_meta: None,
    });
    Ok(())
}

/// Update last_active timestamp for a thread (called on ChatMessage).
/// Also flushes pending_meta (deferred save from ChatStart) on first touch.
async fn touch_thread(active_threads: &ActiveThreads, thread_id: &str, conv_mgr: &ConversationManager) {
    if let Some(claim) = active_threads.lock().await.get_mut(thread_id) {
        claim.last_active = std::time::Instant::now();
        if let Some(meta) = claim.pending_meta.take() {
            conv_mgr.save_meta(thread_id, &meta);
        }
    }
}

async fn thread_locked_error(mgr: &SessionManager, owner_session_id: &str) -> serde_json::Value {
    let host = mgr.get_session_attr(owner_session_id, "hostname").await
        .unwrap_or_else(|| "unknown".to_string());
    let pid = mgr.get_session_attr(owner_session_id, "pid").await
        .unwrap_or_else(|| "?".to_string());
    let cwd = mgr.get_session_attr(owner_session_id, "shell_cwd").await
        .unwrap_or_else(|| "?".to_string());
    let display = format!(
        "Thread active on another session (host={}, pid={}, cwd={})",
        host, pid, cwd
    );
    serde_json::json!({
        "display": display,
        "error": "thread_locked",
    })
}

type SandboxRules = Arc<std::sync::RwLock<HashMap<String, Vec<crate::sandbox_rules::PermitRule>>>>;

type CancelFlags = Arc<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>;

/// Shared runtime options threaded through request handlers.
pub struct ServerOpts {
    pub sandbox_rules: SandboxRules,
    pub config_path: std::path::PathBuf,
    pub daemon_config: std::sync::Arc<std::sync::RwLock<omnish_common::config::DaemonConfig>>,
}

/// Shared state threaded through every message handler.
///
/// Built once per serve loop and shared across all connections.  Holds every
/// manager, registry, and runtime counter so individual handlers can be
/// refactored without growing a new parameter per subsystem.
struct HandlerCtx {
    session_mgr: Arc<SessionManager>,
    llm_holder: SharedLlmBackend,
    task_mgr: Arc<Mutex<TaskManager>>,
    conv_mgr: Arc<ConversationManager>,
    plugin_mgr: Arc<PluginManager>,
    tool_registry: Arc<omnish_daemon::tool_registry::ToolRegistry>,
    formatter_mgr: Arc<omnish_daemon::formatter_mgr::FormatterManager>,
    pending_loops: Arc<Mutex<HashMap<String, AgentLoopState>>>,
    cancel_flags: CancelFlags,
    active_threads: ActiveThreads,
    opts: Arc<ServerOpts>,
    update_cache: Arc<omnish_daemon::update_cache::UpdateCache>,
    plugin_bundler: Arc<omnish_daemon::plugin_bundle::PluginBundler>,
    io_requests: Arc<AtomicU64>,
    io_bytes: Arc<AtomicU64>,
    push_registry: PushRegistry,
}

impl HandlerCtx {
    /// Snapshot the current LLM backend (follows hot-reload across calls).
    fn llm(&self) -> Arc<MultiBackend> {
        self.llm_holder.read().unwrap().clone()
    }
}

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: SharedLlmBackend,
    task_mgr: Arc<Mutex<TaskManager>>,
    conv_mgr: Arc<ConversationManager>,
    plugin_mgr: Arc<PluginManager>,
    tool_registry: Arc<omnish_daemon::tool_registry::ToolRegistry>,
    formatter_mgr: Arc<omnish_daemon::formatter_mgr::FormatterManager>,
    pending_agent_loops: Arc<Mutex<HashMap<String, AgentLoopState>>>,
    /// Cancel flags for running agent loops (keyed by request_id).
    /// Set to true by ChatInterrupt to signal daemon-side loops to stop.
    cancel_flags: CancelFlags,
    active_threads: ActiveThreads,
    opts: Arc<ServerOpts>,
    update_cache: Arc<omnish_daemon::update_cache::UpdateCache>,
    plugin_bundler: Arc<omnish_daemon::plugin_bundle::PluginBundler>,
    pub push_registry: PushRegistry,
}

/// Shallow-merge params into a JSON object. Source keys overwrite target keys.
fn merge_tool_params(target: &mut serde_json::Value, params: &HashMap<String, serde_json::Value>) {
    if let Some(obj) = target.as_object_mut() {
        for (k, v) in params {
            obj.insert(k.clone(), v.clone());
        }
    }
}

/// Compare two DaemonConfig values and return client-relevant changes.
pub fn diff_client_config(old: &omnish_common::config::DaemonConfig, new: &omnish_common::config::DaemonConfig) -> Vec<ConfigChange> {
    let mut changes = Vec::new();
    if old.client.command_prefix != new.client.command_prefix {
        changes.push(ConfigChange { path: "client.command_prefix".into(), value: new.client.command_prefix.clone() });
    }
    if old.client.resume_prefix != new.client.resume_prefix {
        changes.push(ConfigChange { path: "client.resume_prefix".into(), value: new.client.resume_prefix.clone() });
    }
    if old.client.completion_enabled != new.client.completion_enabled {
        changes.push(ConfigChange { path: "client.completion_enabled".into(), value: new.client.completion_enabled.to_string() });
    }
    if old.client.ghost_timeout_ms != new.client.ghost_timeout_ms {
        changes.push(ConfigChange { path: "client.ghost_timeout_ms".into(), value: new.client.ghost_timeout_ms.to_string() });
    }
    if old.client.intercept_gap_ms != new.client.intercept_gap_ms {
        changes.push(ConfigChange { path: "client.intercept_gap_ms".into(), value: new.client.intercept_gap_ms.to_string() });
    }
    if old.client.developer_mode != new.client.developer_mode {
        changes.push(ConfigChange { path: "client.developer_mode".into(), value: new.client.developer_mode.to_string() });
    }
    if old.client.language != new.client.language {
        changes.push(ConfigChange { path: "client.language".into(), value: new.client.language.clone() });
    }
    changes
}

/// Build a full set of client-relevant config changes (for initial push).
pub fn full_client_changes(cfg: &omnish_common::config::DaemonConfig) -> Vec<ConfigChange> {
    vec![
        ConfigChange { path: "client.command_prefix".into(), value: cfg.client.command_prefix.clone() },
        ConfigChange { path: "client.resume_prefix".into(), value: cfg.client.resume_prefix.clone() },
        ConfigChange { path: "client.completion_enabled".into(), value: cfg.client.completion_enabled.to_string() },
        ConfigChange { path: "client.ghost_timeout_ms".into(), value: cfg.client.ghost_timeout_ms.to_string() },
        ConfigChange { path: "client.intercept_gap_ms".into(), value: cfg.client.intercept_gap_ms.to_string() },
        ConfigChange { path: "client.developer_mode".into(), value: cfg.client.developer_mode.to_string() },
        ConfigChange { path: "client.language".into(), value: cfg.client.language.clone() },
    ]
}

/// Execute a daemon-side plugin by spawning a subprocess.
/// Uses the same stdin/stdout JSON protocol as client-side plugins.
/// Returns `(ToolResult, needs_summarization)`.
async fn execute_daemon_plugin(
    executable: &std::path::Path,
    tool_name: &str,
    input: &serde_json::Value,
    proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> (omnish_llm::tool::ToolResult, bool) {
    use tokio::process::Command;
    use tokio::io::AsyncWriteExt;

    let request = serde_json::json!({
        "name": tool_name,
        "input": input,
    });

    let mut cmd = Command::new(executable);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(proxy_url) = proxy {
        cmd.env("HTTP_PROXY", proxy_url)
            .env("HTTPS_PROXY", proxy_url)
            .env("http_proxy", proxy_url)
            .env("https_proxy", proxy_url);
    }
    if let Some(no_proxy_str) = no_proxy {
        cmd.env("NO_PROXY", no_proxy_str)
            .env("no_proxy", no_proxy_str);
    }
    let mut child = match cmd.spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (omnish_llm::tool::ToolResult {
                tool_use_id: String::new(),
                content: format!("Failed to spawn plugin '{}': {}", executable.display(), e),
                is_error: true,
            }, false);
        }
    };

    // Write request to stdin
    if let Some(mut stdin) = child.stdin.take() {
        let data = serde_json::to_string(&request).unwrap();
        let _ = stdin.write_all(data.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        drop(stdin);
    }

    // Wait with timeout
    let timeout = std::time::Duration::from_secs(600);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return (omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Plugin exited with {}: {}", output.status, stderr.trim()),
                    is_error: true,
                }, false);
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            #[derive(serde::Deserialize)]
            struct PluginResponse {
                content: String,
                #[serde(default)]
                is_error: bool,
                #[serde(default)]
                needs_summarization: bool,
            }
            match serde_json::from_str::<PluginResponse>(stdout.trim()) {
                Ok(resp) => (omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: resp.content,
                    is_error: resp.is_error,
                }, resp.needs_summarization),
                Err(e) => (omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Invalid plugin response: {e}"),
                    is_error: true,
                }, false),
            }
        }
        Ok(Err(e)) => (omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Plugin I/O error: {e}"),
            is_error: true,
        }, false),
        Err(_) => {
            // Timeout: the future was dropped, which drops the Child,
            // killing the process. Return error.
            (omnish_llm::tool::ToolResult {
                tool_use_id: String::new(),
                content: "Plugin timed out (600s)".to_string(),
                is_error: true,
            }, false)
        }
    }
}

/// Summarize tool result content using the LLM. Returns the summarized text, or None on failure.
async fn summarize_tool_result(
    backend: &dyn LlmBackend,
    tool_name: &str,
    content: &str,
    prompt_template: &str,
    user_prompt: &str,
) -> Option<String> {
    // Truncate content based on backend's max_content_chars to avoid exceeding context window.
    // Reserve 20% for the prompt template and response overhead.
    let truncated: &str = if let Some(max_chars) = backend.max_content_chars() {
        let limit = max_chars * 4 / 5;
        if content.len() > limit {
            &content[..content.floor_char_boundary(limit)]
        } else {
            content
        }
    } else {
        content
    };
    // Replace {prompt} before {content} to prevent content containing "{prompt}" from being substituted
    let query = prompt_template
        .replace("{prompt}", user_prompt)
        .replace("{content}", truncated);
    let req = omnish_llm::backend::LlmRequest {
        context: String::new(),
        query: Some(query),
        trigger: omnish_llm::backend::TriggerType::Manual,
        session_ids: vec![],
        use_case: omnish_llm::backend::UseCase::Summarize,
        max_content_chars: None,
        system_prompt: None,
        enable_thinking: Some(false),
        tools: vec![],
        extra_messages: vec![],
    };
    match backend.complete(&req).await {
        Ok(resp) => {
            let text: String = resp.content.iter().filter_map(|b| {
                if let omnish_llm::backend::ContentBlock::Text(t) = b { Some(t.as_str()) } else { None }
            }).collect::<Vec<_>>().join("");
            if text.is_empty() { None } else { Some(text) }
        }
        Err(e) => {
            tracing::warn!("Summarization failed for {tool_name}: {e}, using raw content");
            None
        }
    }
}

impl DaemonServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_mgr: Arc<SessionManager>,
        llm_backend: SharedLlmBackend,
        task_mgr: Arc<Mutex<TaskManager>>,
        conv_mgr: Arc<ConversationManager>,
        plugin_mgr: Arc<PluginManager>,
        tool_registry: Arc<omnish_daemon::tool_registry::ToolRegistry>,
        opts: Arc<ServerOpts>,
        formatter_mgr: Arc<omnish_daemon::formatter_mgr::FormatterManager>,
        update_cache: Arc<omnish_daemon::update_cache::UpdateCache>,
        plugin_bundler: Arc<omnish_daemon::plugin_bundle::PluginBundler>,
    ) -> Self {
        Self {
            session_mgr,
            llm_backend,
            task_mgr,
            conv_mgr,
            plugin_mgr,
            tool_registry,
            formatter_mgr,
            pending_agent_loops: Arc::new(Mutex::new(HashMap::new())),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            active_threads: Arc::new(Mutex::new(HashMap::new())),
            opts,
            update_cache,
            plugin_bundler,
            push_registry: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(
        &self,
        addr: &str,
        auth_token: String,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Result<()> {
        let mut server = RpcServer::bind(addr).await?;
        tracing::info!("omnishd listening on {}", addr);

        // Periodically sweep stale pending agent loop entries
        let pending_cleanup = self.pending_agent_loops.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut map = pending_cleanup.lock().await;
                map.retain(|req_id, state| {
                    if state.start.elapsed() > std::time::Duration::from_secs(1800) {
                        tracing::warn!("Cleaning up expired agent loop state: {}", req_id);
                        false
                    } else {
                        true
                    }
                });
            }
        });

        // Counters for IoData traffic over the last minute.
        let io_requests = Arc::new(AtomicU64::new(0));
        let io_bytes = Arc::new(AtomicU64::new(0));

        // Periodically release idle thread claims (safety net: 30m10s) and log IoData stats.
        let idle_threads = self.active_threads.clone();
        let stats_requests = io_requests.clone();
        let stats_bytes = io_bytes.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            let max_idle = std::time::Duration::from_secs(30 * 60 + 10);
            loop {
                interval.tick().await;
                let mut map = idle_threads.lock().await;
                map.retain(|tid, claim| {
                    if claim.last_active.elapsed() > max_idle {
                        tracing::info!("Releasing idle thread claim: {} (session={})", tid, claim.session_id);
                        false
                    } else {
                        true
                    }
                });
                let reqs = stats_requests.swap(0, Ordering::Relaxed);
                let bytes = stats_bytes.swap(0, Ordering::Relaxed);
                tracing::debug!("IoData last 60s: {} requests, {} bytes", reqs, bytes);
            }
        });

        let daemon_config_for_push = self.opts.daemon_config.clone();
        let on_push_connect: Option<OnPushConnect> = Some(Arc::new(move |push_tx: mpsc::Sender<Message>| {
            let config = daemon_config_for_push.clone();
            Box::pin(async move {
                let changes = {
                    let cfg = config.read().unwrap();
                    full_client_changes(&cfg)
                };
                let _ = push_tx.send(Message::ConfigClient { changes }).await;
            })
        }));

        let ctx = Arc::new(HandlerCtx {
            session_mgr: self.session_mgr.clone(),
            llm_holder: self.llm_backend.clone(),
            task_mgr: self.task_mgr.clone(),
            conv_mgr: self.conv_mgr.clone(),
            plugin_mgr: self.plugin_mgr.clone(),
            tool_registry: self.tool_registry.clone(),
            formatter_mgr: self.formatter_mgr.clone(),
            pending_loops: self.pending_agent_loops.clone(),
            cancel_flags: self.cancel_flags.clone(),
            active_threads: self.active_threads.clone(),
            opts: self.opts.clone(),
            update_cache: self.update_cache.clone(),
            plugin_bundler: self.plugin_bundler.clone(),
            io_requests,
            io_bytes,
            push_registry: self.push_registry.clone(),
        });

        server
            .serve(
                move |msg, tx| {
                    let ctx = ctx.clone();
                    Box::pin(async move { handle_message(msg, &ctx, tx).await })
                },
                Some(auth_token),
                tls_acceptor,
                Some(self.push_registry.clone()),
                on_push_connect,
            )
            .await
    }
}

async fn handle_message(
    msg: Message,
    ctx: &HandlerCtx,
    tx: mpsc::Sender<Message>,
) {
    let llm = ctx.llm();
    // Derive chat model name dynamically from current backend (follows hot-reload)
    let chat_model_name = {
        let name = llm.model_name_for_use_case(UseCase::Chat);
        if name == "unavailable" { None } else { Some(name) }
    };

    let mgr = &*ctx.session_mgr;
    match msg {
        Message::SessionStart(s) => {
            if let Err(e) = mgr
                .register(&s.session_id, s.parent_session_id, s.attrs)
                .await
            {
                tracing::error!("register error: {}", e);
            }
            let _ = tx.send(Message::Ack).await;
        }
        Message::SessionEnd(s) => {
            if let Err(e) = mgr.end_session(&s.session_id).await {
                tracing::error!("end_session error: {}", e);
            }
            // Release any threads held by this session
            ctx.active_threads.lock().await.retain(|_, c| c.session_id != s.session_id);
            let _ = tx.send(Message::Ack).await;
        }
        Message::SessionUpdate(su) => {
            if let Err(e) = mgr.update_attrs(&su.session_id, su.timestamp_ms, su.attrs).await {
                tracing::error!("update_attrs error: {}", e);
            }
            let _ = tx.send(Message::Ack).await;
        }
        Message::IoData(io) => {
            ctx.io_requests.fetch_add(1, Ordering::Relaxed);
            ctx.io_bytes.fetch_add(io.data.len() as u64, Ordering::Relaxed);
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
            let _ = tx.send(Message::Ack).await;
        }
        Message::CommandComplete(cc) => {
            if let Err(e) = mgr.receive_command(&cc.session_id, cc.record).await {
                tracing::error!("receive_command error: {}", e);
            }
            // Proactively warm the LLM KV cache if the context prefix changed
            {
                let mgr = ctx.session_mgr.clone();
                let llm = llm.clone();
                let sid = cc.session_id.clone();
                tokio::spawn(async move {
                    try_warmup_kv_cache(&sid, &mgr, &llm).await;
                });
            }
            let _ = tx.send(Message::Ack).await;
        }
        Message::Request(req) => {
            if req.query.starts_with("__cmd:") {
                let result = handle_builtin_command(&req, ctx, &llm).await;
                let content = serde_json::to_string(&result).unwrap_or_else(|_| {
                    r#"{"display":"(serialization error)"}"#.to_string()
                });
                let _ = tx.send(Message::Response(Response {
                    request_id: req.request_id,
                    content,
                    is_streaming: false,
                    is_final: true,
                })).await;
                return;
            }

            let content = match handle_llm_request(&req, mgr, &llm).await {
                Ok(response) => response.text(),
                Err(e) => {
                    tracing::error!("LLM request failed: {}", e);
                    format!("Error: {}", e)
                }
            };

            let _ = tx.send(Message::Response(Response {
                request_id: req.request_id,
                content,
                is_streaming: false,
                is_final: true,
            })).await;
        }
        Message::CompletionRequest(req) => {
            tracing::debug!(
                "CompletionRequest: input={:?} seq={}",
                req.input,
                req.sequence_id
            );
            // Debug shortcut: return canned suggestions for testing
            // Supports: "omnish_debug" (immediate) and "omnish_debug delay <ms>" (delayed)
            let trimmed = req.input.trim();
            if trimmed == "omnish_debug" || trimmed.starts_with("omnish_debug ") {
                // Parse optional delay: "omnish_debug delay 2000" → sleep 2s before responding
                let delay_ms = trimmed.strip_prefix("omnish_debug delay ")
                    .and_then(|s| s.trim().parse::<u64>().ok());
                if let Some(ms) = delay_ms {
                    tracing::info!("omnish_debug delay {}ms, seq={}", ms, req.sequence_id);
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                } else {
                    tracing::info!("omnish_debug matched, returning canned suggestions");
                }
                let _ = tx.send(Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
                    sequence_id: req.sequence_id,
                    suggestions: vec![
                        omnish_protocol::message::CompletionSuggestion {
                            text: format!("{} yes", trimmed),
                            confidence: 1.0,
                        },
                        omnish_protocol::message::CompletionSuggestion {
                            text: format!("{} || echo works", trimmed),
                            confidence: 0.9,
                        },
                    ],
                })).await;
                return;
            }
            let reply = match handle_completion_request(&req, mgr, &llm).await {
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
            };
            let _ = tx.send(reply).await;
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
            let _ = tx.send(Message::Ack).await;
        }
        Message::ChatStart(cs) => {
            handle_chat_start(cs, ctx, &llm, chat_model_name.clone(), tx).await;
        }
        Message::ChatEnd(ce) => {
            tracing::debug!("[ChatEnd] session={} thread={}", ce.session_id, ce.thread_id);
            // Release thread binding
            let mut threads = ctx.active_threads.lock().await;
            if let Some(claim) = threads.get(&ce.thread_id) {
                if claim.session_id == ce.session_id {
                    threads.remove(&ce.thread_id);
                    tracing::debug!("[ChatEnd] released thread={}", ce.thread_id);
                } else {
                    tracing::debug!("[ChatEnd] thread={} owned by different session={}, not releasing",
                        ce.thread_id, claim.session_id);
                }
            } else {
                tracing::debug!("[ChatEnd] thread={} not in active_threads", ce.thread_id);
            }
            let _ = tx.send(Message::Ack).await;
        }
        Message::ChatMessage(cm) => {
            touch_thread(&ctx.active_threads, &cm.thread_id, &ctx.conv_mgr).await;
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let req_id = cm.request_id.clone();
            ctx.cancel_flags.lock().await.insert(req_id.clone(), flag.clone());
            handle_chat_message(cm, ctx, &llm, tx, &flag).await;
            ctx.cancel_flags.lock().await.remove(&req_id);
        }
        Message::ChatToolResult(tr) => {
            touch_thread(&ctx.active_threads, &tr.thread_id, &ctx.conv_mgr).await;
            handle_tool_result(tr, ctx, tx).await;
        }
        Message::ChatInterrupt(ci) => {
            handle_chat_interrupt(ci, ctx, tx).await;
        }
        Message::ConfigQuery => {
            let config = ctx.opts.daemon_config.read().unwrap().clone();
            let plugin_metas = ctx.plugin_mgr.config_meta();
            let clients = mgr.list_clients().await;
            let (mut items, handlers) = crate::config_schema::build_config_items(&config, &plugin_metas, &clients);
            // Inject tool param metadata so the client can offer Select pickers
            // for Plugin / Param name in sandbox rule forms.
            items.push(crate::config_schema::build_tool_params_item(&ctx.tool_registry));
            let _ = tx.send(Message::ConfigResponse { items, handlers }).await;
        }
        Message::ConfigUpdate { changes } => {
            handle_config_update(changes, ctx, tx).await;
        }
        Message::ConfigResponse { .. } | Message::ConfigUpdateResult { .. } | Message::ConfigClient { .. } | Message::TestDisconnect { .. } => {
            // These are daemon→client or transport-layer messages, ignore if received at app layer
            let _ = tx.send(Message::Ack).await;
        }
        Message::UpdateCheck { os, arch, current_version, .. } => {
            ctx.update_cache.register_platform(&os, &arch);
            let reply = if !ctx.update_cache.past_startup_grace() {
                Message::UpdateInfo { latest_version: String::new(), checksum: String::new(), available: false }
            } else {
                match ctx.update_cache.check_update_with_checksum(&os, &arch, &current_version) {
                    Some((version, checksum)) => {
                        Message::UpdateInfo { latest_version: version, checksum, available: true }
                    }
                    None => {
                        Message::UpdateInfo { latest_version: String::new(), checksum: String::new(), available: false }
                    }
                }
            };
            let _ = tx.send(reply).await;
        }
        Message::UpdateRequest { os, arch, version, hostname } => {
            handle_update_request(os, arch, version, hostname, ctx, tx).await;
        }
        Message::PluginSyncCheck { current_checksum, .. } => {
            // Issue #588: if the client's checksum matches our cache, the
            // cache is trivially correct relative to the client and no
            // rebuild is needed. If it disagrees, the cache may be up to
            // 5 minutes stale (the scheduled task's interval); rebuild
            // before announcing to avoid advertising a pre-edit snapshot
            // as the current state. `rebuild` is the single code path for
            // all refresh callers (scheduled task + handler) so they
            // serialize on the same mutex and never duplicate the tar.
            let cached = ctx.plugin_bundler.checksum();
            let daemon_checksum = if cached == current_checksum {
                cached
            } else {
                ctx.plugin_bundler.rebuild().await
            };
            let bundle = ctx.plugin_bundler.snapshot();
            let available = !daemon_checksum.is_empty() && daemon_checksum != current_checksum;
            let _ = tx.send(Message::PluginSyncInfo {
                checksum: daemon_checksum,
                available,
                total_size: bundle.bytes.len() as u64,
            }).await;
        }
        Message::PluginSyncRequest { hostname } => {
            handle_plugin_sync_request(hostname, ctx, tx).await;
        }
        _ => {
            let _ = tx.send(Message::Ack).await;
        }
    }
}

/// Build an empty ChatReady (no history, blank thread_id).  Used for error
/// responses (not_found / thread_locked) and the "no threads yet" case.
fn empty_chat_ready(
    request_id: String,
    chat_model_name: Option<String>,
    error: Option<&str>,
    error_display: Option<String>,
) -> Message {
    Message::ChatReady(ChatReady {
        request_id,
        thread_id: String::new(),
        last_exchange: None,
        earlier_count: 0,
        model_name: chat_model_name,
        history: None,
        thread_host: None,
        thread_cwd: None,
        thread_summary: None,
        error: error.map(String::from),
        error_display,
        sandbox_disabled: None,
        thread_title_word: None,
    })
}

/// Build a successful ChatReady for a resumed thread.  Caller must have
/// already claimed the thread and loaded its raw messages.  Persists the
/// session's current host/cwd as pending_meta on the claim.
async fn build_resumed_chat_ready(
    tid: &str,
    request_id: String,
    session_meta: &ThreadMeta,
    raw_msgs: &[serde_json::Value],
    chat_model_name: &Option<String>,
    ctx: &HandlerCtx,
    llm: &Arc<MultiBackend>,
) -> Message {
    let old_meta = ctx.conv_mgr.load_meta(tid);
    let merged_meta = ThreadMeta {
        host: session_meta.host.clone(),
        cwd: session_meta.cwd.clone(),
        ..old_meta.clone()
    };
    if let Some(claim) = ctx.active_threads.lock().await.get_mut(tid) {
        claim.pending_meta = Some(merged_meta);
    }
    let history_vals = reconstruct_history(raw_msgs, &ctx.tool_registry, &ctx.formatter_mgr).await;
    let history: Vec<String> = history_vals.iter()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .collect();
    let thread_model = old_meta.model.clone().and_then(|model_name| {
        let is_default = llm.chat_default_name() == model_name;
        if is_default { None } else { Some(model_name) }
    });
    let effective_title = old_meta.effective_title_label();
    Message::ChatReady(ChatReady {
        request_id,
        thread_id: tid.to_string(),
        last_exchange: None,
        earlier_count: 0,
        model_name: thread_model.or_else(|| chat_model_name.clone()),
        history: Some(history),
        thread_host: old_meta.host,
        thread_cwd: old_meta.cwd,
        thread_summary: old_meta.summary,
        error: None,
        error_display: None,
        sandbox_disabled: old_meta.sandbox_disabled,
        thread_title_word: effective_title,
    })
}

async fn handle_chat_start(
    cs: ChatStart,
    ctx: &HandlerCtx,
    llm: &Arc<MultiBackend>,
    chat_model_name: Option<String>,
    tx: mpsc::Sender<Message>,
) {
    let mgr = &*ctx.session_mgr;
    let conv_mgr = &ctx.conv_mgr;
    let active_threads = &ctx.active_threads;

    let meta = {
        let host = mgr.get_session_attr(&cs.session_id, "hostname").await;
        let cwd = mgr.get_session_attr(&cs.session_id, "shell_cwd").await;
        ThreadMeta { host, cwd, ..Default::default() }
    };

    // Determine thread_id based on the request:
    //  1. thread_id provided → resume that specific thread
    //  2. new_thread = true   → create a new thread
    //  3. otherwise           → resume latest thread
    let ready = if let Some(ref tid) = cs.thread_id {
        tracing::debug!("[ChatStart] resuming thread={}", tid);
        let raw_msgs = conv_mgr.load_raw_messages(tid);
        tracing::debug!("[ChatStart] loaded {} raw messages for thread={}", raw_msgs.len(), tid);
        if raw_msgs.is_empty() {
            tracing::debug!("[ChatStart] thread not found, returning error");
            empty_chat_ready(cs.request_id, chat_model_name.clone(), Some("not_found"), Some("Conversation not found".to_string()))
        } else if let Err(owner) = try_claim_thread(active_threads, tid, &cs.session_id).await {
            tracing::debug!("[ChatStart] thread locked by session={}", owner);
            let err = thread_locked_error(mgr, &owner).await;
            empty_chat_ready(cs.request_id, chat_model_name.clone(), Some("thread_locked"), err.get("display").and_then(|d| d.as_str()).map(String::from))
        } else {
            tracing::debug!("[ChatStart] claimed thread={}, reconstructing history", tid);
            build_resumed_chat_ready(tid, cs.request_id, &meta, &raw_msgs, &chat_model_name, ctx, llm).await
        }
    } else if cs.new_thread {
        let tid = conv_mgr.create_thread(meta);
        tracing::debug!("[ChatStart] created new thread={}", tid);
        try_claim_thread(active_threads, &tid, &cs.session_id).await.ok();
        Message::ChatReady(ChatReady {
            request_id: cs.request_id,
            thread_id: tid,
            last_exchange: None,
            earlier_count: 0,
            model_name: chat_model_name.clone(),
            history: None,
            thread_host: None,
            thread_cwd: None,
            thread_summary: None,
            error: None,
            error_display: None,
            sandbox_disabled: None,
            thread_title_word: None,
        })
    } else {
        tracing::debug!("[ChatStart] resuming latest thread");
        match conv_mgr.get_latest_thread() {
            Some(tid) => {
                tracing::debug!("[ChatStart] latest thread={}", tid);
                if let Err(owner) = try_claim_thread(active_threads, &tid, &cs.session_id).await {
                    tracing::debug!("[ChatStart] latest thread locked by session={}", owner);
                    let err = thread_locked_error(mgr, &owner).await;
                    empty_chat_ready(cs.request_id, chat_model_name.clone(), Some("thread_locked"), err.get("display").and_then(|d| d.as_str()).map(String::from))
                } else {
                    let raw_msgs = conv_mgr.load_raw_messages(&tid);
                    build_resumed_chat_ready(&tid, cs.request_id, &meta, &raw_msgs, &chat_model_name, ctx, llm).await
                }
            }
            None => {
                tracing::debug!("[ChatStart] no threads found");
                empty_chat_ready(cs.request_id, chat_model_name.clone(), None, None)
            }
        }
    };
    let _ = tx.send(ready).await;
}

async fn handle_chat_interrupt(ci: ChatInterrupt, ctx: &HandlerCtx, tx: mpsc::Sender<Message>) {
    // Clean up pending agent loop and store partial results
    let state = if !ci.request_id.is_empty() {
        ctx.pending_loops.lock().await.remove(&ci.request_id)
    } else {
        None
    };

    if let Some(mut state) = state {
        // Build tool_result content: completed results + "user interrupted" for the rest
        let completed_ids: std::collections::HashSet<String> = state
            .completed_results
            .iter()
            .map(|r| r.tool_use_id.clone())
            .collect();

        let mut result_content: Vec<serde_json::Value> = state
            .completed_results
            .iter()
            .map(|r| serde_json::json!({
                "type": "tool_result",
                "tool_use_id": r.tool_use_id,
                "content": r.content,
                "is_error": r.is_error,
            }))
            .collect();

        // Fill in "user interrupted" for tools not yet completed
        for tc in &state.pending_tool_calls {
            if !completed_ids.contains(&tc.id) {
                result_content.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tc.id,
                    "content": "user interrupted",
                    "is_error": true,
                }));
            }
        }

        persist_unsaved(&mut state, &ctx.conv_mgr, &[
            serde_json::json!({"role": "user", "content": result_content}),
            serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
        ]);
        update_thread_usage(&ctx.conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
    } else if let Some(flag) = ctx.cancel_flags.lock().await.get(&ci.request_id) {
        // Agent loop is running daemon-side tools - signal it to stop.
        // The loop will store partial state to conversation when it detects the flag.
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    } else {
        // Loop already finished - just record the interrupt
        ctx.conv_mgr.append_messages(&ci.thread_id, &[
            serde_json::json!({"role": "user", "content": ci.query}),
            serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
        ]);
    }

    tracing::info!("Chat interrupted by user (thread={}, request={})", ci.thread_id, ci.request_id);
    let _ = tx.send(Message::Ack).await;
}

async fn handle_config_update(
    changes: Vec<ConfigChange>,
    ctx: &HandlerCtx,
    tx: mpsc::Sender<Message>,
) {
    let result = crate::config_schema::apply_config_changes(&ctx.opts.config_path, &changes);
    match result {
        Ok(effects) => {
            // Reload config after successful write
            if let Ok(mut new_config) = omnish_common::config::load_daemon_config() {
                omnish_daemon::task_mgr::inject_task_defaults(&mut new_config.tasks);
                *ctx.opts.daemon_config.write().unwrap() = new_config;
            }
            // Spawn background deploy tasks; results are pushed back as NoticePush.
            if !effects.deploy_targets.is_empty() {
                let omnish_dir = omnish_common::config::omnish_dir();
                let listen_addr = ctx.opts.daemon_config.read().unwrap().listen_addr.clone();
                for (target, kind) in effects.deploy_targets {
                    omnish_daemon::deploy::spawn_deploy(
                        omnish_dir.clone(),
                        target,
                        listen_addr.clone(),
                        ctx.push_registry.clone(),
                        kind,
                    );
                }
            }
            // Spawn background plugin installs; results are pushed as NoticePush.
            if !effects.plugin_installs.is_empty() {
                let omnish_dir = omnish_common::config::omnish_dir();
                for url in effects.plugin_installs {
                    omnish_daemon::plugin_install::spawn_install_plugin(
                        url,
                        omnish_dir.clone(),
                        ctx.push_registry.clone(),
                    );
                }
            }
            let _ = tx.send(Message::ConfigUpdateResult { ok: true, error: None }).await;
        }
        Err(e) => {
            let _ = tx.send(Message::ConfigUpdateResult {
                ok: false,
                error: Some(e.to_string()),
            }).await;
        }
    }
}

/// Outcome of a `stream_chunks` run. `Completed(seq)` = streamed `seq + 1`
/// chunks (including the done marker). `Aborted(seq)` = client disconnected
/// at `seq`. `Err(msg)` = read error; caller should emit the error via
/// `send_update_error`.
enum StreamOutcome {
    Completed(u32),
    Aborted(u32),
    Err(String),
}

/// Shared byte-streaming helper for binary updates and plugin sync (issue
/// #588). Reads from `reader` in 64KB chunks and emits `UpdateChunk`
/// messages: the first chunk carries `total_size` + `checksum`, subsequent
/// chunks carry data only, and a final empty `done: true` chunk closes the
/// stream. Returns `Aborted` if the channel send fails (client disconnect).
async fn stream_chunks<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    total_size: u64,
    checksum: String,
    tx: &mpsc::Sender<Message>,
) -> StreamOutcome {
    use tokio::io::AsyncReadExt;
    let mut seq = 0u32;
    let mut buf = vec![0u8; 65536];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => return StreamOutcome::Err(format!("read error: {}", e)),
        };
        let done = n == 0;
        let chunk = Message::UpdateChunk {
            seq,
            total_size: if seq == 0 { total_size } else { 0 },
            checksum: if seq == 0 { checksum.clone() } else { String::new() },
            data: if done { vec![] } else { buf[..n].to_vec() },
            done,
            error: None,
        };
        if tx.send(chunk).await.is_err() {
            return StreamOutcome::Aborted(seq);
        }
        if done {
            return StreamOutcome::Completed(seq);
        }
        seq += 1;
    }
}

/// Send an error chunk followed by a done marker (2 messages) so the
/// call_stream Ack sentinel is delivered to the client.
async fn send_update_error(tx: &mpsc::Sender<Message>, seq: u32, msg: String) {
    let _ = tx.send(Message::UpdateChunk {
        seq, total_size: 0, checksum: String::new(),
        data: vec![], done: false, error: Some(msg),
    }).await;
    let _ = tx.send(Message::UpdateChunk {
        seq: seq + 1, total_size: 0, checksum: String::new(),
        data: vec![], done: true, error: None,
    }).await;
}

async fn handle_update_request(
    os: String,
    arch: String,
    version: String,
    hostname: String,
    ctx: &HandlerCtx,
    tx: mpsc::Sender<Message>,
) {
    let update_cache = &ctx.update_cache;
    if !update_cache.try_acquire_transfer(&hostname) {
        send_update_error(&tx, 0, "transfer already in progress for this platform, retry later".into()).await;
        return;
    }

    let cached = update_cache.cached_package(&os, &arch);
    match cached {
        Some((cached_ver, path)) if cached_ver == version => {
            tracing::info!("streaming update package {}-{} v{} to {}", os, arch, version, hostname);
            match tokio::fs::File::open(&path).await {
                Ok(file) => {
                    let total_size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
                    let path_for_checksum = path.clone();
                    let checksum = match tokio::task::spawn_blocking(move || {
                        omnish_common::update::checksum(&path_for_checksum)
                    }).await {
                        Ok(Ok(c)) => c,
                        Ok(Err(e)) => { send_update_error(&tx, 0, format!("checksum error: {}", e)).await; return; }
                        Err(e) => { send_update_error(&tx, 0, format!("join error: {}", e)).await; return; }
                    };
                    match stream_chunks(file, total_size, checksum, &tx).await {
                        StreamOutcome::Completed(seq) => {
                            tracing::info!("streaming {}-{} v{} to {} complete ({} chunks)", os, arch, version, hostname, seq + 1);
                        }
                        StreamOutcome::Aborted(seq) => {
                            tracing::warn!("streaming {}-{} v{} to {} aborted (client disconnected at chunk {})", os, arch, version, hostname, seq);
                        }
                        StreamOutcome::Err(msg) => {
                            send_update_error(&tx, 0, msg).await;
                        }
                    }
                }
                Err(e) => {
                    send_update_error(&tx, 0, format!("open error: {}", e)).await;
                }
            }
        }
        _ => {
            send_update_error(&tx, 0, "not available or version mismatch".into()).await;
        }
    }
}

/// Issue #588: stream the in-memory plugin bundle to a client. Reuses the
/// shared `stream_chunks` helper, so the chunk format is identical to the
/// binary update path - the client demultiplexes by which request the
/// stream belongs to, not by any per-chunk discriminator.
async fn handle_plugin_sync_request(
    hostname: String,
    ctx: &HandlerCtx,
    tx: mpsc::Sender<Message>,
) {
    let bundle = ctx.plugin_bundler.snapshot();
    if bundle.bytes.is_empty() {
        send_update_error(&tx, 0, "no plugin bundle available".into()).await;
        return;
    }
    tracing::info!(
        "streaming plugin bundle ({} bytes, checksum={}) to {}",
        bundle.bytes.len(), bundle.checksum, hostname,
    );
    let total_size = bundle.bytes.len() as u64;
    match stream_chunks(bundle.bytes.as_slice(), total_size, bundle.checksum, &tx).await {
        StreamOutcome::Completed(seq) => {
            tracing::info!("plugin bundle to {} complete ({} chunks)", hostname, seq + 1);
        }
        StreamOutcome::Aborted(seq) => {
            tracing::warn!("plugin bundle to {} aborted at chunk {}", hostname, seq);
        }
        StreamOutcome::Err(msg) => {
            send_update_error(&tx, 0, msg).await;
        }
    }
}

/// Shared chat setup: builds tools list and system prompt from plugins.
/// Used by both the actual chat handler and `/template chat`.
struct ChatSetup {
    command_query_tool: omnish_daemon::tools::command_query::CommandQueryTool,
    tools: Vec<omnish_llm::tool::ToolDef>,
    system_prompt: String,
}

/// Serialize a ContentBlock to its JSON representation for the Anthropic messages API.
fn content_block_to_json(block: &ContentBlock) -> serde_json::Value {
    match block {
        ContentBlock::Thinking { thinking: t, signature } => {
            let mut v = serde_json::json!({"type": "thinking", "thinking": t});
            if let Some(sig) = signature {
                v["signature"] = serde_json::Value::String(sig.clone());
            }
            v
        }
        ContentBlock::Text(t) => serde_json::json!({"type": "text", "text": t}),
        ContentBlock::ToolUse(tc) => {
            let mut v = serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.input,
            });
            if let Some(obj) = v.as_object_mut() {
                for (k, val) in &tc.extra {
                    obj.insert(k.clone(), val.clone());
                }
            }
            v
        }
    }
}

async fn build_chat_setup(mgr: &SessionManager, tool_registry: &omnish_daemon::tool_registry::ToolRegistry) -> ChatSetup {
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(
        commands,
        stream_reader,
    );

    let tools = tool_registry.all_defs();

    // Load base chat prompt, then apply user overrides from chat.override.json
    let pm = load_chat_prompt();
    let system_prompt = pm.build();

    ChatSetup { command_query_tool, tools, system_prompt }
}

async fn handle_chat_message(
    cm: ChatMessage,
    ctx: &HandlerCtx,
    llm: &Arc<MultiBackend>,
    tx: mpsc::Sender<Message>,
    cancel_flag: &Arc<std::sync::atomic::AtomicBool>,
) {
    let mgr = &*ctx.session_mgr;
    let conv_mgr = &ctx.conv_mgr;
    let tool_registry = &ctx.tool_registry;

    // Handle model override
    if let Some(ref model_name) = cm.model {
        let mut meta = conv_mgr.load_meta(&cm.thread_id);
        meta.model = Some(model_name.clone());
        conv_mgr.save_meta(&cm.thread_id, &meta);
    }

    // Model-only message (no query) - just acknowledge
    if cm.query.is_empty() {
        let _ = tx.send(Message::Ack).await;
        return;
    }

    // Resolve per-thread model override for backend selection
    let meta = conv_mgr.load_meta(&cm.thread_id);
    let use_case = UseCase::Chat;
    let effective_backend: Arc<dyn LlmBackend> = meta.model.as_ref()
        .and_then(|name| llm.get_backend_by_name(name))
        .unwrap_or_else(|| llm.get_backend(use_case));

    let max_context_chars = effective_backend.max_content_chars();

    let ChatSetup { command_query_tool, tools, system_prompt } =
        build_chat_setup(mgr, tool_registry).await;

    // Get session attrs from client probes (cwd, platform, os_version, etc.)
    let session_attrs = mgr.get_session_attrs(&cm.session_id).await;

    // Build system-reminder and append to system prompt (no recent commands in chat mode;
    // the agent can query commands via tools)
    let reminder = command_query_tool.build_system_reminder(&cm.session_id, 5, &session_attrs, false);

    // Detect system-reminder changes (dev aid: compare with previous)
    let mut meta = conv_mgr.load_meta(&cm.thread_id);
    if let Some(ref prev) = meta.system_reminder {
        if *prev != reminder {
            tracing::info!("[system-reminder changed] thread={}", cm.thread_id);
        }
    }
    meta.system_reminder = Some(reminder.clone());
    conv_mgr.save_meta(&cm.thread_id, &meta);

    let full_system_prompt = format!("{}\n\n{}", system_prompt, reminder);

    // Load prior conversation history as raw JSON
    let mut extra_messages = conv_mgr.load_raw_messages(&cm.thread_id);

    // Sanitize orphaned tool_use blocks that can appear when a ChatInterrupt
    // races with a new ChatMessage (both are dispatched concurrently).
    if omnish_daemon::conversation_mgr::sanitize_orphaned_tool_use(&mut extra_messages) {
        tracing::warn!("Sanitized orphaned tool_use blocks before chat (thread={})", cm.thread_id);
        conv_mgr.replace_messages(&cm.thread_id, &extra_messages);
    }

    // Strip internal metadata fields that must not be sent to the LLM API
    for msg in &mut extra_messages {
        if let Some(obj) = msg.as_object_mut() {
            obj.remove("_usage");
            obj.remove("_model");
        }
    }
    let prior_len = extra_messages.len();

    // User message (clean, without system-reminder)
    let user_msg = serde_json::json!({"role": "user", "content": cm.query});
    extra_messages.push(user_msg.clone());

    // Persist user message immediately so /resume works even if the agent loop
    // hasn't finished (each message is handled in its own spawned task, so
    // ChatEnd can race with the agent loop).
    conv_mgr.append_messages(&cm.thread_id, &[user_msg]);

    // Wrap raw JSON messages in TaggedMessage carriers (cache hints set in Task 6).
    let extra_messages: Vec<omnish_llm::backend::TaggedMessage> = extra_messages
        .into_iter()
        .map(|content| omnish_llm::backend::TaggedMessage::new(
            content,
            omnish_llm::backend::CacheHint::None,
        ))
        .collect();

    let llm_req = LlmRequest {
        context: String::new(),
        query: None,
        trigger: TriggerType::Manual,
        session_ids: vec![cm.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        system_prompt: Some(omnish_llm::backend::CachedText {
            text: full_system_prompt,
            cache: omnish_llm::backend::CacheHint::Long,
        }),
        enable_thinking: Some(true), // Enable thinking mode for chat
        tools,
        extra_messages,
    };

    let state = AgentLoopState {
        llm_req,
        saved_up_to: prior_len + 1, // user message already persisted
        pending_tool_calls: vec![],
        completed_results: vec![],
        iteration: 0,
        cm,
        start: std::time::Instant::now(),
        command_query_tool,
        effective_backend,
        llm_retries: 0,
        cumulative_usage: Default::default(),
        last_response_usage: Default::default(),
        last_model: String::new(),
    };

    run_agent_loop(state, ctx, tx, cancel_flag).await;
}

/// Handle a ChatToolResult from the client - accumulate results, resume when all are received.
async fn handle_tool_result(
    tr: ChatToolResult,
    ctx: &HandlerCtx,
    tx: mpsc::Sender<Message>,
) {
    let tool_registry = &ctx.tool_registry;
    let formatter_mgr = &ctx.formatter_mgr;
    let pending_loops = &ctx.pending_loops;
    let cancel_flags = &ctx.cancel_flags;
    let mut map = pending_loops.lock().await;
    let state = match map.get_mut(&tr.request_id) {
        Some(s) => s,
        None => {
            tracing::warn!("No pending agent loop for request_id={}", tr.request_id);
            let _ = tx.send(Message::Ack).await;
            return;
        }
    };

    // Add the received client-side tool result
    let tool_call_id = tr.tool_call_id.clone();
    let mut content = tr.content;

    // LLM summarization for client-side tool results
    if tr.needs_summarization && !tr.is_error {
        if let Some(tc) = state.pending_tool_calls.iter().find(|tc| tc.id == tool_call_id) {
            if let Some(prompt_template) = tool_registry.summarization_prompt(&tc.name) {
                let user_prompt = tc.input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(summary) = summarize_tool_result(
                    state.effective_backend.as_ref(), &tc.name, &content, &prompt_template, user_prompt,
                ).await {
                    content = summary;
                }
            }
        }
    }

    state.completed_results.push(omnish_llm::tool::ToolResult {
        tool_use_id: tr.tool_call_id.clone(),
        content,
        is_error: tr.is_error,
    });

    // Generate immediate ChatToolStatus for this result
    if let Some(result) = state.completed_results.iter().find(|r| r.tool_use_id == tool_call_id) {
        if let Some(tc) = state.pending_tool_calls.iter().find(|tc| tc.id == tool_call_id) {
            let formatter_name = tool_registry.formatter_name(&tc.name);
            let display_name = tool_registry.display_name(&tc.name).to_string();
            let fmt_out = formatter_mgr.format(&formatter_name, &omnish_plugin::formatter::FormatInput {
                tool_name: tc.name.clone(),
                params: tc.input.clone(),
                output: result.content.clone(),
                is_error: result.is_error,
            }).await;
            let status_icon = if result.is_error { StatusIcon::Error } else { StatusIcon::Success };
            let _ = tx.send(Message::ChatToolStatus(ChatToolStatus {
                request_id: state.cm.request_id.clone(),
                thread_id: state.cm.thread_id.clone(),
                tool_name: tc.name.clone(),
                tool_call_id: Some(tc.id.clone()),
                status: String::new(),
                status_icon: Some(status_icon),
                display_name: Some(display_name),
                param_desc: Some(tool_registry.status_text(&tc.name, &tc.input)),
                result_compact: Some(fmt_out.result_compact),
                result_full: Some(fmt_out.result_full),
            })).await;
        }
    }

    // Check if all tool calls are now completed
    let completed_ids: std::collections::HashSet<String> = state
        .completed_results
        .iter()
        .map(|r| r.tool_use_id.clone())
        .collect();
    let all_complete = state.pending_tool_calls.iter().all(|tc| completed_ids.contains(&tc.id));

    if !all_complete {
        // More results expected - keep waiting (status already sent via tx)
        return;
    }

    // All tool calls complete - remove state and continue agent loop
    let mut state = map.remove(&tr.request_id).unwrap();
    drop(map);

    let result_content: Vec<serde_json::Value> = state
        .completed_results
        .iter()
        .map(|r| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": r.tool_use_id,
                "content": r.content,
                "is_error": r.is_error,
            })
        })
        .collect();
    state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
        serde_json::json!({
            "role": "user",
            "content": result_content,
        }),
        omnish_llm::backend::CacheHint::None,
    ));

    // Clear pending state for next iteration
    state.pending_tool_calls.clear();
    state.completed_results.clear();
    state.iteration += 1;

    // Continue agent loop - register cancel flag so ChatInterrupt can signal it
    let req_id = state.cm.request_id.clone();
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancel_flags.lock().await.insert(req_id.clone(), flag.clone());
    run_agent_loop(state, ctx, tx, &flag).await;
    cancel_flags.lock().await.remove(&req_id);
}

/// Persist unsaved messages, sanitizing any trailing orphaned tool_use blocks.
///
/// If the last unsaved assistant message contains tool_use blocks without
/// corresponding tool_result, this adds "user interrupted" results so the
/// persisted conversation is always valid for LLM replay.
///
/// Note: when called after a tx.send() failure mid-tool-execution, any
/// already-completed tool results in the caller's local `tool_results` vec
/// have not yet been merged into `extra_messages`. This function marks all
/// tool_use IDs as "user interrupted", losing those results. This is
/// acceptable for error recovery (client disconnect) - the alternative of
/// threading partial results through every call site adds complexity for a
/// rare edge case where the client is already gone.
fn persist_unsaved_sanitized(
    state: &mut AgentLoopState,
    conv_mgr: &omnish_daemon::conversation_mgr::ConversationManager,
) {
    // Check whether the tail of extra_messages ends with an assistant message
    // containing tool_use blocks (i.e. no tool_result follows).
    let last_is_tool_use = state.llm_req.extra_messages.last().is_some_and(|msg| {
        msg.content.get("role").and_then(|r| r.as_str()) == Some("assistant")
            && msg.content.get("content").and_then(|c| c.as_array()).is_some_and(|blocks| {
                blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            })
    });

    if last_is_tool_use {
        // Extract tool_use ids from the assistant message
        let tool_ids: Vec<String> = state.llm_req.extra_messages.last().unwrap()
            .content
            .get("content").unwrap().as_array().unwrap()
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .filter_map(|b| b.get("id").and_then(|id| id.as_str()).map(String::from))
            .collect();

        let result_content: Vec<serde_json::Value> = tool_ids.iter().map(|id| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": "user interrupted",
                "is_error": true,
            })
        }).collect();

        state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
            serde_json::json!({
                "role": "user",
                "content": result_content,
            }),
            omnish_llm::backend::CacheHint::None,
        ));
    }

    persist_unsaved(state, conv_mgr, &[
        serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
    ]);
    update_thread_usage(conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
}

/// Persist unsaved messages from the agent loop to the conversation thread.
/// Appends `suffix` messages (e.g. event markers) after the unsaved slice.
/// Updates `saved_up_to` to reflect what has been persisted.
fn persist_unsaved(
    state: &mut AgentLoopState,
    conv_mgr: &omnish_daemon::conversation_mgr::ConversationManager,
    suffix: &[serde_json::Value],
) {
    let mut to_store: Vec<serde_json::Value> = state.llm_req.extra_messages[state.saved_up_to..]
        .iter()
        .map(|m| m.content.clone())
        .collect();
    to_store.extend_from_slice(suffix);
    if !to_store.is_empty() {
        conv_mgr.append_messages(&state.cm.thread_id, &to_store);
    }
    state.saved_up_to = state.llm_req.extra_messages.len();
}

/// Update thread meta with the most recent API call's usage after an agent loop run.
/// Resets totals when the model changes.
///
/// `last_response` - the final LLM API call's usage (stored as usage_last, added to usage_total).
/// `cumulative`    - total usage across all API calls in this agent loop iteration
///                   (added to the running thread total).
/// `model`         - config backend name used to detect model switches.
fn update_thread_usage(
    conv_mgr: &omnish_daemon::conversation_mgr::ConversationManager,
    thread_id: &str,
    last_response: &omnish_llm::backend::Usage,
    cumulative: &omnish_llm::backend::Usage,
    model: &str,
) {
    if model.is_empty() {
        return;
    }
    use omnish_daemon::conversation_mgr::ThreadUsage;
    let mut meta = conv_mgr.load_meta(thread_id);
    let last = ThreadUsage {
        input_tokens: last_response.input_tokens,
        output_tokens: last_response.output_tokens,
        cache_read_input_tokens: last_response.cache_read_input_tokens,
        cache_creation_input_tokens: last_response.cache_creation_input_tokens,
    };
    // Reset totals on model switch
    let same_model = meta.last_model.as_deref() == Some(model);
    let total = if same_model {
        let prev = meta.usage_total.get_or_insert(ThreadUsage::default());
        ThreadUsage {
            input_tokens: prev.input_tokens + cumulative.input_tokens,
            output_tokens: prev.output_tokens + cumulative.output_tokens,
            cache_read_input_tokens: prev.cache_read_input_tokens + cumulative.cache_read_input_tokens,
            cache_creation_input_tokens: prev.cache_creation_input_tokens + cumulative.cache_creation_input_tokens,
        }
    } else {
        // Model switched - start fresh with this loop's cumulative usage
        ThreadUsage {
            input_tokens: cumulative.input_tokens,
            output_tokens: cumulative.output_tokens,
            cache_read_input_tokens: cumulative.cache_read_input_tokens,
            cache_creation_input_tokens: cumulative.cache_creation_input_tokens,
        }
    };
    meta.usage_last = Some(last);
    meta.usage_total = Some(total);
    meta.last_model = Some(model.to_string());
    conv_mgr.save_meta(thread_id, &meta);
}

/// Apply cache hints for the chat agent loop's message tail.
/// Resets all hints to None, then marks the last 2 messages as Long.
/// Called before each LLM call so newly-appended messages get fresh marks
/// (without accumulating beyond the budget).
fn mark_chat_message_hints(messages: &mut [omnish_llm::backend::TaggedMessage]) {
    for m in messages.iter_mut() {
        m.cache = omnish_llm::backend::CacheHint::None;
    }
    let len = messages.len();
    for i in 0..2.min(len) {
        messages[len - 1 - i].cache = omnish_llm::backend::CacheHint::Long;
    }
}

/// Core agent loop: calls LLM, executes tools, pauses on client-side tools.
/// Used by both `handle_chat_message` (initial) and `handle_tool_result` (resumption).
/// Messages are sent incrementally through `tx` as they're produced (streaming).
async fn run_agent_loop(
    mut state: AgentLoopState,
    ctx: &HandlerCtx,
    tx: mpsc::Sender<Message>,
    cancel_flag: &Arc<std::sync::atomic::AtomicBool>,
) {
    let conv_mgr = &ctx.conv_mgr;
    let plugin_mgr = &ctx.plugin_mgr;
    let tool_registry = &ctx.tool_registry;
    let formatter_mgr = &ctx.formatter_mgr;
    let pending_loops = &ctx.pending_loops;
    let opts = &ctx.opts;
    let backend = &state.effective_backend;

    let max_iterations = 100;

    for iteration in state.iteration..max_iterations {
        // Check if user interrupted (Ctrl+C)
        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("Agent loop cancelled by user at iteration {} (thread={})", iteration, state.cm.thread_id);
            persist_unsaved_sanitized(&mut state, conv_mgr);
            return;
        }

        // Apply cache hints fresh each iteration: agent loop appends new messages,
        // and last-N markers must roll forward without accumulating beyond budget.
        mark_chat_message_hints(&mut state.llm_req.extra_messages);

        match backend.complete(&state.llm_req).await {
            Ok(response) => {
                state.llm_retries = 0;
                // Track last response and accumulate totals across iterations
                if let Some(ref u) = response.usage {
                    state.last_response_usage = omnish_llm::backend::Usage {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        cache_read_input_tokens: u.cache_read_input_tokens,
                        cache_creation_input_tokens: u.cache_creation_input_tokens,
                    };
                    state.cumulative_usage.input_tokens += u.input_tokens;
                    state.cumulative_usage.output_tokens += u.output_tokens;
                    state.cumulative_usage.cache_read_input_tokens += u.cache_read_input_tokens;
                    state.cumulative_usage.cache_creation_input_tokens += u.cache_creation_input_tokens;
                }
                state.last_model = backend.name().to_string();

                if response.stop_reason == StopReason::ToolUse {
                    let tool_calls = response.tool_calls();
                    if tool_calls.is_empty() {
                        break;
                    }

                    // Build assistant message preserving original block order
                    // (thinking, text, tool_use - order matters for DeepSeek-compatible APIs)
                    let assistant_content: Vec<serde_json::Value> = response
                        .content
                        .iter()
                        .map(content_block_to_json)
                        .collect();
                    state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                        serde_json::json!({
                            "role": "assistant",
                            "content": assistant_content,
                        }),
                        omnish_llm::backend::CacheHint::None,
                    ));

                    // Send LLM's text blocks to client immediately
                    for block in &response.content {
                        if let ContentBlock::Text(text) = block {
                            let text = unwrap_thinking_tags(text);
                            if !text.is_empty()
                                && tx.send(Message::ChatToolStatus(ChatToolStatus {
                                    request_id: state.cm.request_id.clone(),
                                    thread_id: state.cm.thread_id.clone(),
                                    tool_name: String::new(),
                                    status: text,
                                    tool_call_id: None,
                                    status_icon: None,
                                    display_name: None,
                                    param_desc: None,
                                    result_compact: None,
                                    result_full: None,
                                })).await.is_err() {
                                    persist_unsaved_sanitized(&mut state, conv_mgr);
                                    return;
                                }
                        }
                    }

                    // Execute daemon-side tools immediately, forward all client-side tools
                    let mut tool_results = Vec::new();
                    let mut has_client_tools = false;
                    let mut cancelled = false;
                    for tc in &tool_calls {
                        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                            cancelled = true;
                            break;
                        }
                        let display_name = tool_registry.display_name(&tc.name).to_string();
                        let param_desc = tool_registry.status_text(&tc.name, &tc.input);

                        // Send pre-execution tool status immediately
                        if tx.send(Message::ChatToolStatus(ChatToolStatus {
                            request_id: state.cm.request_id.clone(),
                            thread_id: state.cm.thread_id.clone(),
                            tool_name: tc.name.clone(),
                            tool_call_id: Some(tc.id.clone()),
                            status: String::new(),
                            status_icon: Some(StatusIcon::Running),
                            display_name: Some(display_name),
                            param_desc: Some(param_desc),
                            result_compact: None,
                            result_full: None,
                        })).await.is_err() {
                            persist_unsaved_sanitized(&mut state, conv_mgr);
                            return;
                        }

                        let ptype = tool_registry.plugin_type(&tc.name);
                        if ptype == Some(PluginType::ClientTool) {
                            // Client-side tool: forward to client for parallel execution
                            let mut merged_input = tc.input.clone();
                            if let Some(override_params) = tool_registry.override_params(&tc.name) {
                                merge_tool_params(&mut merged_input, &override_params);
                            }
                            {
                                let plugins_config = &opts.daemon_config.read().unwrap().plugins;
                                if let Some(config_params) = plugins_config.get(&tc.name) {
                                    let filtered: HashMap<String, serde_json::Value> = config_params
                                        .iter()
                                        .filter(|(k, _)| k.as_str() != "enabled")
                                        .map(|(k, v)| (k.clone(), v.clone()))
                                        .collect();
                                    merge_tool_params(&mut merged_input, &filtered);
                                }
                            }
                            let matched_rule = {
                                let rules = opts.sandbox_rules.read().unwrap();
                                crate::sandbox_rules::check_bypass(
                                    rules.get(&tc.name).map(|v| v.as_slice()).unwrap_or(&[]),
                                    &tc.input,
                                ).map(|s| s.to_string())
                            };
                            if let Some(ref rule) = matched_rule {
                                tracing::warn!(
                                    "sandbox bypass: tool={}, rule='{}', input={}",
                                    tc.name, rule,
                                    serde_json::to_string(&tc.input).unwrap_or_default(),
                                );
                            }
                            let thread_sandbox_off = conv_mgr
                                .load_meta(&state.cm.thread_id)
                                .sandbox_disabled
                                .unwrap_or(false);
                            if thread_sandbox_off {
                                tracing::warn!(
                                    "thread sandbox disabled: thread={}, tool={}",
                                    state.cm.thread_id, tc.name
                                );
                            }
                            if tx.send(Message::ChatToolCall(ChatToolCall {
                                request_id: state.cm.request_id.clone(),
                                thread_id: state.cm.thread_id.clone(),
                                tool_name: tc.name.clone(),
                                tool_call_id: tc.id.clone(),
                                input: serde_json::to_string(&merged_input).unwrap_or_default(),
                                plugin_name: tool_registry.plugin_name(&tc.name).unwrap_or_else(|| "builtin".to_string()),
                                sandboxed: matched_rule.is_none() && !thread_sandbox_off,
                            })).await.is_err() {
                                persist_unsaved_sanitized(&mut state, conv_mgr);
                                return;
                            }
                            has_client_tools = true;
                        } else {
                            // Daemon-side tool: execute directly
                            let mut merged_input = tc.input.clone();
                            if let Some(override_params) = tool_registry.override_params(&tc.name) {
                                merge_tool_params(&mut merged_input, &override_params);
                            }
                            {
                                let plugins_config = &opts.daemon_config.read().unwrap().plugins;
                                if let Some(config_params) = plugins_config.get(&tc.name) {
                                    let filtered: HashMap<String, serde_json::Value> = config_params
                                        .iter()
                                        .filter(|(k, _)| k.as_str() != "enabled")
                                        .map(|(k, v)| (k.clone(), v.clone()))
                                        .collect();
                                    merge_tool_params(&mut merged_input, &filtered);
                                }
                            }

                            let (mut result, needs_summarization) = if tool_registry.plugin_type(&tc.name).is_some() {
                                if let Some(exe) = plugin_mgr.plugin_executable(&tc.name) {
                                    let (proxy, no_proxy) = {
                                        let dc = opts.daemon_config.read().unwrap();
                                        (dc.proxy.http_proxy.clone(), dc.proxy.no_proxy.clone())
                                    };
                                    execute_daemon_plugin(&exe, &tc.name, &merged_input, proxy.as_deref(), no_proxy.as_deref()).await
                                } else {
                                    (omnish_llm::tool::ToolResult {
                                        tool_use_id: String::new(),
                                        content: format!("Unknown daemon tool: {}", tc.name),
                                        is_error: true,
                                    }, false)
                                }
                            } else if tool_registry.is_known(&tc.name) {
                                (state.command_query_tool.execute(&tc.name, &merged_input), false)
                            } else {
                                (omnish_llm::tool::ToolResult {
                                    tool_use_id: String::new(),
                                    content: format!("Unknown tool: {}", tc.name),
                                    is_error: true,
                                }, false)
                            };
                            result.tool_use_id = tc.id.clone();

                            // LLM summarization: if the tool requested it and has a prompt template
                            if needs_summarization && !result.is_error {
                                if let Some(prompt_template) = tool_registry.summarization_prompt(&tc.name) {
                                    let user_prompt = tc.input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                                    if let Some(summary) = summarize_tool_result(
                                        state.effective_backend.as_ref(), &tc.name, &result.content, &prompt_template, user_prompt,
                                    ).await {
                                        result.content = summary;
                                    }
                                }
                            }

                            // Post-execution: send update ChatToolStatus with formatted results immediately
                            let post_display = tool_registry.display_name(&tc.name).to_string();
                            let post_fmt_name = tool_registry.formatter_name(&tc.name);
                            let post_out = formatter_mgr.format(&post_fmt_name, &omnish_plugin::formatter::FormatInput {
                                tool_name: tc.name.clone(),
                                params: tc.input.clone(),
                                output: result.content.clone(),
                                is_error: result.is_error,
                            }).await;
                            let post_icon = if result.is_error { StatusIcon::Error } else { StatusIcon::Success };
                            if tx.send(Message::ChatToolStatus(ChatToolStatus {
                                request_id: state.cm.request_id.clone(),
                                thread_id: state.cm.thread_id.clone(),
                                tool_name: tc.name.clone(),
                                tool_call_id: Some(tc.id.clone()),
                                status: String::new(),
                                status_icon: Some(post_icon),
                                display_name: Some(post_display),
                                param_desc: Some(tool_registry.status_text(&tc.name, &tc.input)),
                                result_compact: Some(post_out.result_compact),
                                result_full: Some(post_out.result_full),
                            })).await.is_err() {
                                persist_unsaved_sanitized(&mut state, conv_mgr);
                                return;
                            }

                            tool_results.push(result);
                        }
                    }

                    if cancelled {
                        // Cancelled mid-tool-execution - store partial state
                        tracing::info!("Agent loop cancelled during tool execution (thread={})", state.cm.thread_id);
                        // Build tool results for completed tools + "user interrupted" for the rest
                        let completed_ids: std::collections::HashSet<&str> = tool_results
                            .iter()
                            .map(|r| r.tool_use_id.as_str())
                            .collect();
                        let mut result_content: Vec<serde_json::Value> = tool_results
                            .iter()
                            .map(|r| serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": r.content,
                                "is_error": r.is_error,
                            }))
                            .collect();
                        for tc in &tool_calls {
                            if !completed_ids.contains(tc.id.as_str()) {
                                result_content.push(serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": tc.id,
                                    "content": "user interrupted",
                                    "is_error": true,
                                }));
                            }
                        }
                        state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                            serde_json::json!({
                                "role": "user",
                                "content": result_content,
                            }),
                            omnish_llm::backend::CacheHint::None,
                        ));
                        persist_unsaved(&mut state, conv_mgr, &[
                            serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
                        ]);
                        update_thread_usage(conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
                        return;
                    }

                    if has_client_tools {
                        // Persist accumulated messages so they survive if the client
                        // disconnects or the daemon restarts while waiting for tool results.
                        persist_unsaved(&mut state, conv_mgr, &[]);

                        // Pause loop - client will execute tools in parallel and send results back
                        state.pending_tool_calls = tool_calls.iter().map(|tc| (*tc).clone()).collect();
                        state.completed_results = tool_results;
                        state.iteration = iteration;
                        let request_id = state.cm.request_id.clone();
                        pending_loops.lock().await.insert(request_id, state);
                        return; // tx dropped → Ack sent by spawn_connection
                    }

                    // All tools were daemon-side - build tool_result and continue
                    let result_content: Vec<serde_json::Value> = tool_results
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": r.tool_use_id,
                                "content": r.content,
                                "is_error": r.is_error,
                            })
                        })
                        .collect();
                    state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                        serde_json::json!({
                            "role": "user",
                            "content": result_content,
                        }),
                        omnish_llm::backend::CacheHint::None,
                    ));

                    continue;
                }

                // EndTurn or MaxTokens - extract final text and store
                let text = response.text();
                tracing::info!(
                    "Chat LLM completed in {:?} ({} tool iterations, thread={})",
                    state.start.elapsed(),
                    iteration,
                    state.cm.thread_id
                );
                // Push final assistant response preserving original block order
                let has_thinking = response.content.iter().any(|b| matches!(b, ContentBlock::Thinking { .. }));
                let assistant_msg = if has_thinking {
                    let content: Vec<serde_json::Value> = response.content.iter()
                        .map(content_block_to_json)
                        .collect();
                    serde_json::json!({ "role": "assistant", "content": content })
                } else {
                    serde_json::json!({ "role": "assistant", "content": text })
                };
                state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                    assistant_msg,
                    omnish_llm::backend::CacheHint::None,
                ));
                // Store new messages without system-reminder in user message
                persist_unsaved(&mut state, conv_mgr, &[]);
                update_thread_usage(conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
                let _ = tx.send(Message::ChatResponse(ChatResponse {
                    request_id: state.cm.request_id.clone(),
                    thread_id: state.cm.thread_id.clone(),
                    content: thinking_to_markdown(&text),
                })).await;
                return;
            }
            Err(e) => {
                let err_str = e.to_string();
                let is_connection = err_str.contains("connection")
                    || err_str.contains("tls")
                    || err_str.contains("close_notify")
                    || err_str.contains("UnexpectedEof")
                    || err_str.contains("reset by peer")
                    || err_str.contains("broken pipe");

                if is_connection && state.llm_retries < 2 {
                    state.llm_retries += 1;
                    let backoff = std::time::Duration::from_secs(5 * state.llm_retries as u64);
                    tracing::warn!(
                        "LLM connection error (retry {}/2, thread={}): {} - retrying in {}s",
                        state.llm_retries, state.cm.thread_id, err_str, backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                    continue;
                }

                tracing::error!("Chat LLM failed: {}", e);

                // User-visible message: include truncated error string
                let display_err: &str = if err_str.len() > 200 {
                    &err_str[..err_str.floor_char_boundary(200)]
                } else {
                    &err_str
                };
                let user_msg = if is_connection {
                    format!("Connection to the AI service was lost: {}. Your progress has been saved - you can continue by sending another message.", display_err)
                } else {
                    format!("AI service returned an error: {}. Your progress has been saved - you can continue by sending another message.", display_err)
                };

                // Persist a short event marker (like the cancel paths) so the
                // LLM knows the exchange was interrupted without bloating context.
                state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                    serde_json::json!({
                        "role": "assistant",
                        "content": "<event>api error</event>",
                    }),
                    omnish_llm::backend::CacheHint::None,
                ));
                persist_unsaved(&mut state, conv_mgr, &[]);
                update_thread_usage(conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);

                let _ = tx.send(Message::ChatResponse(ChatResponse {
                    request_id: state.cm.request_id.clone(),
                    thread_id: state.cm.thread_id.clone(),
                    content: user_msg,
                })).await;
                return;
            }
        }
    }

    // Exhausted iterations - store what we have
    tracing::warn!(
        "Agent loop exhausted {} iterations (thread={})",
        max_iterations,
        state.cm.thread_id
    );
    let text = "(Agent reached maximum tool call limit)".to_string();
    state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
        serde_json::json!({
            "role": "assistant",
            "content": text,
        }),
        omnish_llm::backend::CacheHint::None,
    ));
    persist_unsaved(&mut state, conv_mgr, &[]);
    update_thread_usage(conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
    let _ = tx.send(Message::ChatResponse(ChatResponse {
        request_id: state.cm.request_id,
        thread_id: state.cm.thread_id,
        content: text,
    })).await;
}

async fn try_warmup_kv_cache(
    session_id: &str,
    mgr: &SessionManager,
    llm: &Arc<MultiBackend>,
) {
    let backend = llm;

    let max_chars = backend.get_max_content_chars(UseCase::Completion);

    let sections = match mgr.check_and_warmup_sections(session_id, max_chars).await {
        Ok(Some(s)) => s,
        Ok(None) => return, // prefix stable, no warmup needed
        Err(e) => {
            tracing::debug!("KV cache warmup context check failed: {}", e);
            return;
        }
    };

    let (system_prompt, user_input) =
        omnish_llm::template::build_completion_parts("", 0);
    let extra_messages = build_completion_extra_messages(&sections, &user_input);
    let req = LlmRequest {
        context: String::new(),
        query: None,
        trigger: TriggerType::Manual,
        session_ids: vec![session_id.to_string()],
        use_case: UseCase::Completion,
        max_content_chars: max_chars,
        system_prompt: Some(omnish_llm::backend::CachedText {
            text: system_prompt,
            cache: omnish_llm::backend::CacheHint::Long,
        }),
        enable_thinking: Some(false), // Disable thinking for completion
        tools: vec![],
        extra_messages,
    };

    match backend.complete(&req).await {
        Ok(_) => tracing::debug!("KV cache warmup completed for session {}", session_id),
        Err(e) => tracing::debug!("KV cache warmup failed for session {}: {}", session_id, e),
    }
}

/// Build the user message's content blocks for a completion-style request.
///
/// Layout: `[stable_prefix, remainder, query]` with `cache_control` on the
/// stable_prefix block (cache_pos=0). Between warmups, Block 0 is byte-stable
/// so Anthropic's KV cache hits on the cached breakpoint.
fn build_completion_extra_messages(
    sections: &omnish_context::recent::CompletionSections,
    query: &str,
) -> Vec<omnish_llm::backend::TaggedMessage> {
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let has_stable = !sections.stable_prefix.is_empty();
    if has_stable {
        blocks.push(serde_json::json!({
            "type": "text",
            "text": sections.stable_prefix,
        }));
    }
    if !sections.remainder.is_empty() {
        blocks.push(serde_json::json!({
            "type": "text",
            "text": sections.remainder,
        }));
    }
    blocks.push(serde_json::json!({
        "type": "text",
        "text": query,
    }));
    let content = serde_json::json!({
        "role": "user",
        "content": blocks,
    });
    vec![omnish_llm::backend::TaggedMessage {
        content,
        cache: omnish_llm::backend::CacheHint::Long,
        cache_pos: has_stable.then_some(0),
    }]
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


/// Helper to create a command response with only a display string.
fn cmd_display(s: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "display": s.into() })
}

/// Reconstruct structured history entries from stored raw LLM messages.
///
/// Iterates over raw messages and produces a JSON array with typed entries:
/// - `user_input`: a user message with string content
/// - `llm_text`: assistant text that accompanies tool_use blocks
/// - `tool_status`: a formatted tool call + result pair
/// - `response`: assistant final text-only response
/// - `separator`: marks end of an exchange
async fn reconstruct_history(
    raw_messages: &[serde_json::Value],
    tool_registry: &omnish_daemon::tool_registry::ToolRegistry,
    formatter_mgr: &omnish_daemon::formatter_mgr::FormatterManager,
) -> Vec<serde_json::Value> {
    use std::collections::HashMap as HM;

    let mut entries = Vec::new();
    // Map from tool_use_id → (tool_name, input_json)
    let mut pending_tools: HM<String, (String, serde_json::Value)> = HM::new();

    for msg in raw_messages {
        let role = msg["role"].as_str().unwrap_or("");
        match role {
            "user" => {
                if let Some(text) = msg["content"].as_str() {
                    // Plain user input
                    entries.push(serde_json::json!({
                        "type": "user_input",
                        "text": text,
                    }));
                } else if let Some(arr) = msg["content"].as_array() {
                    // tool_result array
                    for block in arr {
                        if block["type"].as_str() == Some("tool_result") {
                            let tool_use_id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                            let output = block["content"].as_str().unwrap_or("").to_string();
                            let is_error = block["is_error"].as_bool().unwrap_or(false);

                            if let Some((tool_name, input)) = pending_tools.remove(&tool_use_id) {
                                let display_name = tool_registry.display_name(&tool_name).to_string();
                                let fmt_name = tool_registry.formatter_name(&tool_name);
                                let fmt_out = formatter_mgr.format(&fmt_name, &omnish_plugin::formatter::FormatInput {
                                    tool_name: tool_name.clone(),
                                    params: input.clone(),
                                    output,
                                    is_error,
                                }).await;
                                let param_desc = tool_registry.status_text(&tool_name, &input);
                                let icon_str = if is_error { "error" } else { "success" };
                                entries.push(serde_json::json!({
                                    "type": "tool_status",
                                    "tool_name": tool_name,
                                    "tool_call_id": tool_use_id,
                                    "status_icon": icon_str,
                                    "display_name": display_name,
                                    "param_desc": param_desc,
                                    "result_compact": fmt_out.result_compact,
                                    "result_full": fmt_out.result_full,
                                }));
                            }
                        }
                    }
                }
            }
            "assistant" => {
                if let Some(arr) = msg["content"].as_array() {
                    let has_tool_use = arr.iter().any(|b| b["type"].as_str() == Some("tool_use"));
                    if has_tool_use {
                        // Assistant message with tool calls
                        for block in arr {
                            match block["type"].as_str() {
                                Some("text") => {
                                    let text = block["text"].as_str().unwrap_or("");
                                    if !text.is_empty() {
                                        entries.push(serde_json::json!({
                                            "type": "llm_text",
                                            "text": text,
                                        }));
                                    }
                                }
                                Some("tool_use") => {
                                    let id = block["id"].as_str().unwrap_or("").to_string();
                                    let name = block["name"].as_str().unwrap_or("").to_string();
                                    let input = block["input"].clone();
                                    pending_tools.insert(id, (name, input));
                                }
                                _ => {}
                            }
                        }
                    } else {
                        // Assistant message with only text blocks → final response
                        let text: String = arr.iter()
                            .filter_map(|b| {
                                if b["type"].as_str() == Some("text") {
                                    b["text"].as_str().map(|s| s.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !text.is_empty() {
                            entries.push(serde_json::json!({
                                "type": "response",
                                "text": text,
                            }));
                            entries.push(serde_json::json!({"type": "separator"}));
                        }
                    }
                } else if let Some(text) = msg["content"].as_str() {
                    // Simple string content assistant message → final response
                    if !text.is_empty() {
                        entries.push(serde_json::json!({
                            "type": "response",
                            "text": text,
                        }));
                        entries.push(serde_json::json!({"type": "separator"}));
                    }
                }
            }
            _ => {}
        }
    }

    entries
}

/// Build JSON response for /resume, including thread model if non-default.
async fn build_resume_response(
    thread_id: &str,
    conv_mgr: &ConversationManager,
    tool_registry: &omnish_daemon::tool_registry::ToolRegistry,
    formatter_mgr: &omnish_daemon::formatter_mgr::FormatterManager,
    llm_backend: &Arc<MultiBackend>,
) -> serde_json::Value {
    let raw_msgs = conv_mgr.load_raw_messages(thread_id);
    let history = reconstruct_history(&raw_msgs, tool_registry, formatter_mgr).await;
    let mut json = serde_json::json!({
        "thread_id": thread_id,
        "history": history,
    });
    // Include thread model if it differs from the default chat backend
    let meta = conv_mgr.load_meta(thread_id);
    if let Some(ref model_name) = meta.model {
        let is_default = llm_backend.chat_default_name() == model_name;
        if !is_default {
            json["model"] = serde_json::json!(model_name);
        }
    }
    json
}

async fn handle_builtin_command(req: &Request, ctx: &HandlerCtx, llm_backend: &Arc<MultiBackend>) -> serde_json::Value {
    let mgr = &*ctx.session_mgr;
    let task_mgr = &*ctx.task_mgr;
    let conv_mgr = &ctx.conv_mgr;
    let tool_registry = &*ctx.tool_registry;
    let formatter_mgr = &*ctx.formatter_mgr;
    let active_threads = &ctx.active_threads;
    let sub = req.query.strip_prefix("__cmd:").unwrap_or("");

    // Build system-reminder for context display
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(commands, stream_reader);
    let session_attrs = mgr.get_session_attrs(&req.session_id).await;
    let reminder = command_query_tool.build_system_reminder(&req.session_id, 5, &session_attrs, false);

    // Handle /context chat:<thread_id> - show conversation context + system-reminder
    if let Some(thread_id) = sub.strip_prefix("context chat:") {
        let msgs = conv_mgr.load_raw_messages(thread_id);
        let mut output = format!("Chat thread: {}\n\n", thread_id);
        if msgs.is_empty() {
            output.push_str("(empty conversation)\n\n");
        } else {
            for msg in &msgs {
                let role = msg["role"].as_str().unwrap_or("unknown");
                let label = if role == "user" { "User" } else { "Assistant" };
                let text = ConversationManager::extract_text_public(msg);
                if !text.is_empty() {
                    output.push_str(&format!("[{}] {}\n\n", label, text));
                }
            }
        }
        output.push_str(&reminder);
        return cmd_display(output);
    }

    // Handle /context chat (without thread_id) - show only system-reminder
    if sub == "context chat" {
        return cmd_display(reminder);
    }

    // Handle /context <scenario> for showing context for different scenarios
    if let Some(scenario) = sub.strip_prefix("context ") {
        return cmd_display(handle_context_scenario(scenario, req, mgr, llm_backend, conv_mgr).await);
    }
    // Handle /resume [n] for resuming a specific conversation (by index)
    if let Some(idx_str) = sub.strip_prefix("resume ") {
        let idx: usize = match idx_str.trim().parse::<usize>() {
            Ok(i) if i >= 1 => i - 1, // Convert 1-based to 0-based
            Ok(_) => return cmd_display("Invalid index: must be >= 1"),
            Err(_) => return cmd_display("Invalid index: not a number"),
        };
        return match conv_mgr.get_thread_by_index(idx) {
            Some(thread_id) => build_resume_response(&thread_id, conv_mgr, tool_registry, formatter_mgr, llm_backend).await,
            None => cmd_display("Invalid index: out of bounds"),
        };
    }
    // Handle /resume without index (resume latest = /resume 1)
    if sub == "resume" {
        return match conv_mgr.get_thread_by_index(0) {
            Some(thread_id) => build_resume_response(&thread_id, conv_mgr, tool_registry, formatter_mgr, llm_backend).await,
            None => cmd_display("No conversations yet. Start a chat with :"),
        };
    }
    // Handle /conversations del <thread_id> - delete a conversation by thread ID
    if let Some(tid) = sub.strip_prefix("conversations del ") {
        let tid = tid.trim();
        if conv_mgr.delete_thread(tid) {
            return serde_json::json!({
                "display": format!("Deleted conversation {}", &tid[..8.min(tid.len())]),
                "deleted_thread_id": tid,
            });
        } else {
            return cmd_display("Conversation not found");
        }
    }
    // Handle /model - list available backends with selected flag
    if sub == "models" || sub.starts_with("models ") {
        let thread_id = sub.strip_prefix("models ").unwrap_or("").trim();

        let backends = llm_backend.list_backends();
        if backends.is_empty() {
            return cmd_display("No LLM backends configured".to_string());
        }

        // Determine which backend is selected for this thread
        let selected_name = if !thread_id.is_empty() {
            let meta = conv_mgr.load_meta(thread_id);
            meta.model.unwrap_or_else(|| llm_backend.chat_default_name().to_string())
        } else {
            llm_backend.chat_default_name().to_string()
        };

        let models: Vec<serde_json::Value> = backends.iter().map(|b| {
            serde_json::json!({
                "name": b.name,
                "model": b.model,
                "selected": b.name == selected_name,
            })
        }).collect();

        return serde_json::json!({
            "display": "",
            "models": models,
        });
    }
    if let Some(name) = sub.strip_prefix("template ") {
        return cmd_display(handle_template(name, mgr, tool_registry).await);
    }
    if sub == "template" {
        return cmd_display(format!(
            "Usage: /template <{}> [> file.txt]",
            omnish_llm::template::TEMPLATE_NAMES.join("|")
        ));
    }
    // Handle /thread rename - set or clear a sticky user title override.
    // Payload form: `thread rename <name>:<tid>` to set, `thread rename:<tid>` to clear.
    if let Some(rest) = sub.strip_prefix("thread rename") {
        let (value, tid) = match rest.strip_prefix(':') {
            Some(tid) => (None, tid),
            None => match rest.strip_prefix(' ') {
                Some(after_space) => match after_space.rfind(':') {
                    Some(idx) => {
                        let name = after_space[..idx].to_string();
                        let tid = &after_space[idx + 1..];
                        (Some(name), tid)
                    }
                    None => {
                        return cmd_display("Usage: __cmd:thread rename[ <name>]:<tid>");
                    }
                },
                None => {
                    return cmd_display("Usage: __cmd:thread rename[ <name>]:<tid>");
                }
            },
        };
        if tid.is_empty() {
            return cmd_display("Error: missing thread_id");
        }
        if !conv_mgr.thread_exists(tid) {
            return cmd_display("Error: thread not found");
        }
        conv_mgr.set_title_override(tid, value.clone());
        let meta = conv_mgr.load_meta(tid);
        let effective = meta.effective_title_label().unwrap_or_default();
        let display = match value {
            Some(name) => format!("thread renamed: {}", name),
            None => match meta.title_word {
                Some(ref w) => format!("override cleared (title: {})", w),
                None => "override cleared".to_string(),
            },
        };
        return serde_json::json!({
            "display": display,
            "title_label": effective,
        });
    }
    // Handle /thread sandbox - query or set per-thread sandbox override.
    // Queries embed the thread_id as ":<tid>" suffix, matching /context chat.
    if let Some(rest) = sub.strip_prefix("thread sandbox") {
        let rest = rest.trim_start();
        let (action, tid) = if let Some(tid) = rest.strip_prefix(":") {
            ("query", tid)
        } else if let Some(tid) = rest.strip_prefix("on:") {
            ("on", tid)
        } else if let Some(tid) = rest.strip_prefix("off:") {
            ("off", tid)
        } else {
            return cmd_display("Usage: __cmd:thread sandbox[ on|off]:<tid>");
        };
        if tid.is_empty() {
            return cmd_display("Error: missing thread_id");
        }
        if !conv_mgr.thread_exists(tid) {
            return cmd_display("Error: thread not found");
        }
        let display = match action {
            "on" => {
                conv_mgr.set_sandbox_disabled(tid, false);
                format!("sandbox enabled for thread {}", &tid[..8.min(tid.len())])
            }
            "off" => {
                conv_mgr.set_sandbox_disabled(tid, true);
                format!("sandbox disabled for thread {}", &tid[..8.min(tid.len())])
            }
            _ => {
                let off = conv_mgr.load_meta(tid).sandbox_disabled.unwrap_or(false);
                format!("sandbox: {}", if off { "off" } else { "on" })
            }
        };
        return cmd_display(display);
    }
    match sub {
        "context" => {
            // Default to completion context (most common LLM use case)
            match mgr.build_completion_context(&req.session_id, None).await {
                Ok(ctx) => cmd_display(ctx),
                Err(e) => cmd_display(format!("Error: {}", e)),
            }
        }
        "sessions" => cmd_display(mgr.format_sessions_list(&req.session_id).await),
        "conversations stats" => format_thread_stats(conv_mgr, active_threads, &req.session_id).await,
        s if s == "conversations" || s.starts_with("conversations ") => {
            let args: Vec<&str> = s.strip_prefix("conversations")
                .map(|a| a.split_whitespace().collect())
                .unwrap_or_default();
            let mut limit: Option<usize> = None;
            let mut current_host_only = false;
            for a in &args {
                match *a {
                    "all" => limit = Some(usize::MAX),
                    "current_host" => current_host_only = true,
                    n => if let Ok(x) = n.parse::<usize>() { limit = Some(x); }
                }
            }
            let host_filter = if current_host_only {
                mgr.get_session_attr(&req.session_id, "hostname").await
            } else {
                None
            };
            format_conversations_json(conv_mgr, active_threads, limit, host_filter).await
        }
        "session" => match get_session_debug_info(&req.session_id, mgr).await {
            Ok(info) => cmd_display(info),
            Err(e) => cmd_display(format!("Error: {}", e)),
        },
        sub if sub == "commands" || sub.starts_with("commands ") => {
            let args = sub.strip_prefix("commands").unwrap_or("").trim();
            let count: usize = args.parse().unwrap_or(30);
            cmd_display(command_query_tool.list_history(count))
        }
        sub if sub == "command" || sub.starts_with("command ") => {
            let args = sub.strip_prefix("command").unwrap_or("").trim();
            match args.parse::<usize>() {
                Ok(seq) => cmd_display(command_query_tool.get_command_detail(seq)),
                Err(_) => cmd_display("Usage: /debug command <seq>".to_string()),
            }
        }
        "daemon" => {
            let mut lines = vec![format!("omnish-daemon {}", omnish_common::VERSION)];
            lines.push(String::new());
            let tm = task_mgr.lock().await;
            lines.push(tm.format_list());
            cmd_display(lines.join("\n"))
        }
        sub if sub == "tasks" || sub.starts_with("tasks ") => {
            cmd_display(handle_tasks_command(sub, task_mgr).await)
        }
        other => cmd_display(format!("Unknown command: {}", other)),
    }
}

async fn handle_template(name: &str, mgr: &SessionManager, tool_registry: &omnish_daemon::tool_registry::ToolRegistry) -> String {
    match name {
        "chat" => {
            let ChatSetup { tools, system_prompt, .. } =
                build_chat_setup(mgr, tool_registry).await;

            let tools_json: Vec<String> = tools
                .iter()
                .map(|t| {
                    serde_json::to_string_pretty(&serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    }))
                    .unwrap_or_default()
                })
                .collect();

            format!(
                "=== System Prompt ===\n{}\n\n\
                 === Tools ===\n{}\n\n\
                 === Message Format ===\n\
                 [conversation history if resuming thread...]\n\
                 User: {{query}}\n\
                 [agent loop: tool_use → tool_result, up to 5 iterations]\n\
                 Assistant: {{response}}",
                system_prompt,
                tools_json.join("\n"),
            )
        }
        other => {
            match omnish_llm::template::template_by_name(other) {
                Some(t) => t,
                None => format!(
                    "Unknown template: {}\nAvailable: {}",
                    other,
                    omnish_llm::template::TEMPLATE_NAMES.join(", ")
                ),
            }
        }
    }
}

/// Format the list of conversations as JSON with display string, thread_ids, and locked status.
/// When `limit` is None, defaults to 20 most recent threads.
/// When `host_filter` is Some, only threads whose meta.host matches are included.
async fn format_conversations_json(
    conv_mgr: &Arc<ConversationManager>,
    active_threads: &ActiveThreads,
    limit: Option<usize>,
    host_filter: Option<String>,
) -> serde_json::Value {
    let all_conversations = conv_mgr.list_conversations();
    let conversations: Vec<_> = if let Some(ref host) = host_filter {
        all_conversations
            .into_iter()
            .filter(|(tid, _, _, _)| {
                conv_mgr.load_meta(tid).host.as_deref() == Some(host.as_str())
            })
            .collect()
    } else {
        all_conversations
    };
    if conversations.is_empty() {
        return cmd_display("No conversations yet. Start a chat with :");
    }

    let total = conversations.len();
    let limit = limit.unwrap_or(20);
    let shown: Vec<_> = conversations.into_iter().take(limit).collect();

    let locked_set: std::collections::HashSet<String> = {
        let threads = active_threads.lock().await;
        threads.keys().cloned().collect()
    };

    let mut output = String::from("Conversations:\n");
    let mut thread_ids = Vec::new();
    let mut locked_threads = Vec::new();
    for (i, (thread_id, modified, exchange_count, last_question)) in shown.into_iter().enumerate() {
        let time_ago = format_relative_time(modified);

        let meta = conv_mgr.load_meta(&thread_id);
        let is_locked = locked_set.contains(&thread_id);

        let truncate_display = |s: &str, max: usize| -> String {
            let single_line = s.replace('\n', " ");
            if single_line.chars().count() > max {
                let end: String = single_line.chars().take(max - 3).collect();
                format!("{}...", end)
            } else {
                single_line
            }
        };

        if let Some(ref title) = meta.summary {
            output.push_str(&format!(
                "  [{}] {} | {} turns | {} | {}\n",
                i + 1,
                time_ago,
                exchange_count,
                truncate_display(title, 30),
                truncate_display(&last_question, 30),
            ));
        } else {
            output.push_str(&format!(
                "  [{}] {} | {} turns | {}\n",
                i + 1,
                time_ago,
                exchange_count,
                truncate_display(&last_question, 50),
            ));
        }

        thread_ids.push(thread_id);
        locked_threads.push(is_locked);
    }
    if total > thread_ids.len() {
        output.push_str(&format!("  ({} total, showing {}. Use /thread list N to see more)\n", total, thread_ids.len()));
    }
    serde_json::json!({
        "display": output,
        "thread_ids": thread_ids,
        "locked_threads": locked_threads,
    })
}

/// Aggregate token usage stats for a thread.
///
/// Returns display string with per-thread stats:
/// - Thread list (like `/thread list` but without last user input)
/// - Current model (from last exchange)
/// - Last exchange tokens (input+output)
/// - Total tokens (sum of all exchanges, reset on model switch)
/// - Cache hit rate (cache_read / input)
async fn format_thread_stats(conv_mgr: &Arc<ConversationManager>, active_threads: &ActiveThreads, session_id: &str) -> serde_json::Value {
    // Find the thread currently claimed by this session, if any.
    let current_thread_id: Option<String> = {
        let threads = active_threads.lock().await;
        threads
            .iter()
            .find(|(_, claim)| claim.session_id == session_id)
            .map(|(tid, _)| tid.clone())
    };

    // If there is an active thread for this session, show only that thread.
    if let Some(ref thread_id) = current_thread_id {
        let meta = conv_mgr.load_meta(thread_id);
        let conversations = conv_mgr.list_conversations();
        let entry = conversations
            .into_iter()
            .find(|(tid, _, _, _)| tid == thread_id);

        let (exchange_count, modified) = entry
            .map(|(_, modified, exchange_count, _)| (exchange_count, modified))
            .unwrap_or((0, std::time::UNIX_EPOCH));

        let time_ago = format_relative_time(modified);
        let title_display = meta.summary.as_deref().unwrap_or("untitled");

        let mut output = format!(
            "Thread Stats:\n  {} | {} turns | {} [active]\n",
            time_ago,
            exchange_count,
            title_display,
        );

        if let (Some(last_model), Some(last), Some(total)) =
            (meta.last_model.as_deref(), meta.usage_last.as_ref(), meta.usage_total.as_ref())
        {
            let total_input = total.input_tokens + total.cache_read_input_tokens + total.cache_creation_input_tokens;
            let cache_rate = if total_input > 0 {
                (total.cache_read_input_tokens as f64 / total_input as f64) * 100.0
            } else {
                0.0
            };
            output.push_str(&format!(
                "  model: {} | context: {} | total: {} | cache: {:.1}%\n",
                last_model,
                format_tokens(last.input_tokens + last.cache_read_input_tokens + last.cache_creation_input_tokens + last.output_tokens),
                format_tokens(total.input_tokens + total.cache_read_input_tokens + total.cache_creation_input_tokens + total.output_tokens),
                cache_rate,
            ));
        } else {
            output.push_str("  (no usage data)\n");
        }
        if meta.sandbox_disabled == Some(true) {
            output.push_str("  sandbox: off\n");
        }

        return cmd_display(output);
    }

    // No active thread: fall back to showing all threads.
    let conversations = conv_mgr.list_conversations();
    if conversations.is_empty() {
        return cmd_display("No conversations yet. Start a chat with :");
    }

    let locked_set: std::collections::HashSet<String> = {
        let threads = active_threads.lock().await;
        threads.keys().cloned().collect()
    };

    let mut output = String::from("Thread Stats:\n");
    let truncate_display = |s: &str, max: usize| -> String {
        let single_line = s.replace('\n', " ");
        if single_line.chars().count() > max {
            let end: String = single_line.chars().take(max - 3).collect();
            format!("{}...", end)
        } else {
            single_line
        }
    };

    for (i, (thread_id, modified, exchange_count, _last_question)) in conversations.into_iter().enumerate() {
        // _last_question is intentionally not shown here; stats display uses title + usage data instead.
        let time_ago = format_relative_time(modified);
        let meta = conv_mgr.load_meta(&thread_id);
        let is_locked = locked_set.contains(&thread_id);

        // Thread header (like /thread list but without last user input)
        let title_display = meta.summary.as_deref().unwrap_or("untitled");
        output.push_str(&format!(
            "  [{}] {} | {} turns | {}",
            i + 1,
            time_ago,
            exchange_count,
            truncate_display(title_display, 40),
        ));
        if is_locked {
            output.push_str(" [active]");
        }
        output.push('\n');

        // Read usage stats from thread meta
        if let (Some(last_model), Some(last), Some(total)) =
            (meta.last_model.as_deref(), meta.usage_last.as_ref(), meta.usage_total.as_ref())
        {
            let total_input = total.input_tokens + total.cache_read_input_tokens + total.cache_creation_input_tokens;
            let cache_rate = if total_input > 0 {
                (total.cache_read_input_tokens as f64 / total_input as f64) * 100.0
            } else {
                0.0
            };
            output.push_str(&format!(
                "       model: {} | context: {} | total: {} | cache: {:.1}%\n",
                last_model,
                format_tokens(last.input_tokens + last.cache_read_input_tokens + last.cache_creation_input_tokens + last.output_tokens),
                format_tokens(total.input_tokens + total.cache_read_input_tokens + total.cache_creation_input_tokens + total.output_tokens),
                cache_rate,
            ));
        } else {
            output.push_str("       (no usage data)\n");
        }
        if meta.sandbox_disabled == Some(true) {
            output.push_str("       sandbox: off\n");
        }
    }
    cmd_display(output)
}

/// Format token count with K/M suffix for readability.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a SystemTime as a relative time string (e.g., "12s ago", "1h ago", "2d ago").
fn format_relative_time(time: std::time::SystemTime) -> String {
    let now = std::time::SystemTime::now();
    let duration = match now.duration_since(time) {
        Ok(d) => d,
        Err(_) => return "now".to_string(),
    };

    let secs = duration.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 604800 {
        format!("{}d ago", secs / 86400)
    } else {
        // For older dates, show as absolute date
        chrono::DateTime::<chrono::Utc>::from(time)
            .format("%Y-%m-%d")
            .to_string()
    }
}

/// Handle /context <scenario> to show context for different use cases.
async fn handle_context_scenario(scenario: &str, req: &Request, mgr: &SessionManager, llm_backend: &Arc<MultiBackend>, conv_mgr: &Arc<ConversationManager>) -> String {
    match scenario {
        "chat" | "analysis" => {
            // Return system-reminder (terminal context) when not in a specific chat thread
            match resolve_chat_context(req, mgr, None).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        "auto-complete" | "completion" => {
            // Auto-complete context - uses CompletionFormatter with elastic window
            let max_chars = llm_backend.get_max_content_chars(UseCase::Completion);
            match mgr.build_completion_context(&req.session_id, max_chars).await {
                Ok(ctx) => ctx,
                Err(e) => format!("Error: {}", e),
            }
        }
        "daily-notes" => {
            let notes_dir = omnish_common::config::omnish_dir().join("notes");
            // Show yesterday's context (same as the scheduled job)
            let yesterday = (chrono::Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
            let ctx = omnish_daemon::daily_notes::build_daily_context(&notes_dir, &yesterday);
            if ctx.is_empty() {
                "No hourly summaries for yesterday".to_string()
            } else {
                ctx
            }
        }
        "hourly-notes" | "hourly" => {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let since_ms = now_ms.saturating_sub(4 * 3600 * 1000);
            let commands = mgr.collect_recent_commands(since_ms).await;
            let window_ago = std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(4 * 3600))
                .unwrap_or(std::time::UNIX_EPOCH);
            let conversations_md = conv_mgr.collect_recent_conversations_md(window_ago);
            if commands.is_empty() && conversations_md.is_empty() {
                return "No commands or conversations in the past 4 hours".to_string();
            }
            let (ctx, _table_md) = omnish_daemon::hourly_summary::build_hourly_context(&commands, &conversations_md);
            ctx
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
        info.push_str("Status: Active\n");
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

        info.push_str("\nCommand statistics:\n");
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
    backend: &Arc<MultiBackend>,
) -> Result<omnish_llm::backend::LlmResponse> {
    let use_case = UseCase::Chat;
    let max_context_chars = backend.get_max_content_chars(use_case);
    let context = resolve_chat_context(req, mgr, max_context_chars).await?;

    let llm_req = LlmRequest {
        context,
        query: Some(req.query.clone()),
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        system_prompt: None,
        enable_thinking: Some(true), // Enable thinking mode for chat
        tools: vec![],
        extra_messages: vec![],
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

            // Log thinking length and content
            if let Some(ref thinking) = response.thinking() {
                tracing::info!("LLM thinking: {} chars", thinking.len());
                tracing::debug!("LLM thinking content: {}", thinking);
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
    backend: &Arc<MultiBackend>,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let use_case = UseCase::Completion;
    let max_context_chars = backend.get_max_content_chars(use_case);

    // Get previous context for prefix match ratio calculation
    let last_context = mgr.get_last_completion_context().await;

    let sections = mgr
        .build_completion_sections(&req.session_id, max_context_chars)
        .await?;
    let context = if sections.stable_prefix.is_empty() && sections.remainder.is_empty() {
        String::new()
    } else {
        format!("{}{}", sections.stable_prefix, sections.remainder)
    };

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

    let (system_prompt, user_input) =
        omnish_llm::template::build_completion_parts(&req.input, req.cursor_pos);
    let context_for_sample = context.clone();
    let prompt_for_sample = format!("{}\n\n{}\n\n{}", system_prompt, context, user_input);

    let prompt_words = system_prompt.split_whitespace().count()
        + context.split_whitespace().count()
        + user_input.split_whitespace().count();

    let extra_messages = build_completion_extra_messages(&sections, &user_input);
    let llm_req = LlmRequest {
        context: String::new(),
        query: None,
        trigger: TriggerType::Manual,
        session_ids: vec![req.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        system_prompt: Some(omnish_llm::backend::CachedText {
            text: system_prompt,
            cache: omnish_llm::backend::CacheHint::Long,
        }),
        enable_thinking: Some(false), // Disable thinking for completion requests
        tools: vec![],
        extra_messages,
    };
    tracing::info!(
        "Completion LLM request started (session={}, sequence_id={}, input_len={}, prompt_words={})",
        req.session_id, req.sequence_id, req.input.len(), prompt_words
    );

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
            let cache_info = if let Some(ref u) = response.usage {
                let total_input = u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
                if total_input > 0 {
                    format!(", cache={:.1}%", (u.cache_read_input_tokens as f64 / total_input as f64) * 100.0)
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            if duration_secs > 1.5 {
                // Slow requests (>1.5s) logged as WARN so tracing colors them
                tracing::warn!(
                    "Completion LLM request completed in {} (session={}, model={}, sequence_id={}, input_len={}, prompt_words={}{})",
                    duration_str, req.session_id, response.model, req.sequence_id, req.input.len(), prompt_words, cache_info
                );
            } else {
                tracing::info!(
                    "Completion LLM request completed in {} (session={}, model={}, sequence_id={}, input_len={}, prompt_words={}{})",
                    duration_str, req.session_id, response.model, req.sequence_id, req.input.len(), prompt_words, cache_info
                );
            }
            tracing::debug!("Completion LLM raw response: {:?}", response.text());

            // Log thinking length and content
            if let Some(ref thinking) = response.thinking() {
                tracing::info!("Completion LLM thinking: {} chars", thinking.len());
                tracing::debug!("Completion LLM thinking content: {}", thinking);
            }
        }
        Err(e) => {
            tracing::warn!(
                "Completion LLM request failed after {} (session={}, sequence_id={}, input_len={}, prompt_words={}, error={})",
                duration_str, req.session_id, req.sequence_id, req.input.len(), prompt_words, e
            );
        }
    }

    let response = result?;
    let suggestions = parse_completion_suggestions(&response.text())?;

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
        context: context_for_sample,
        prompt: prompt_for_sample,
        suggestions: suggestion_texts,
        input: req.input.clone(),
        cwd: req.cwd.clone(),
        latency_ms: duration.as_millis() as u64,
        accepted: false,
        created_at: std::time::Instant::now(),
    }).await;

    Ok(suggestions)
}

// Extract the first balanced `[...]` from the content. Tracks JSON string
// context so that `]` or `[` inside quoted strings (including escaped quotes)
// do not terminate the span. Returns None when no balanced pair exists.
fn extract_first_json_array(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'[')?;

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, &b) in bytes[start..].iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match b {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_completion_suggestions(
    content: &str,
) -> Result<Vec<omnish_protocol::message::CompletionSuggestion>> {
    let trimmed = content.trim();

    // Only accept a JSON array in one of the two documented shapes. Anything
    // else (prose, a bare word, a JSON object like `{"error": "...", ...}`,
    // etc.) is treated as a malformed response and discarded. Issue #586.
    let Some(json_str) = extract_first_json_array(trimmed) else {
        return Ok(Vec::new());
    };

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

    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use omnish_llm::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason};
    use omnish_protocol::message::CompletionRequest as ProtoCompletionRequest;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::time::{sleep, Duration};

    #[test]
    fn test_unwrap_thinking_tags() {
        // Leading tag - strips tags, keeps content
        assert_eq!(
            unwrap_thinking_tags("<thinking>\nContinue.\n</thinking>\n\nRun #27："),
            "Continue.\n\nRun #27："
        );
        assert_eq!(
            unwrap_thinking_tags("<think>\nLet me analyze.\n</think>\nHere is the answer."),
            "Let me analyze.\nHere is the answer."
        );
        assert_eq!(unwrap_thinking_tags("<thinking>foo</thinking>"), "foo");
        // Leading whitespace before tag - still matches
        assert_eq!(unwrap_thinking_tags("  <thinking>bar</thinking>"), "bar");
        // Unclosed tag - treats rest as content
        assert_eq!(unwrap_thinking_tags("<thinking>rest of text"), "rest of text");
        // No tags
        assert_eq!(unwrap_thinking_tags("plain text"), "plain text");
        // Tag NOT at start - no change (avoids false positives)
        assert_eq!(
            unwrap_thinking_tags("before <thinking>inner</thinking> after"),
            "before <thinking>inner</thinking> after"
        );
    }

    #[test]
    fn test_thinking_to_markdown() {
        // Leading tag - converts to heading section
        assert_eq!(
            thinking_to_markdown("<thinking>\nContinue.\n</thinking>\n\nRun #27："),
            "# Thinking\nContinue.\n\n# Response\nRun #27："
        );
        assert_eq!(
            thinking_to_markdown("<think>\nLine 1\nLine 2\n</think>\nAnswer"),
            "# Thinking\nLine 1\nLine 2\n\n# Response\nAnswer"
        );
        // Empty thinking - removed
        assert_eq!(thinking_to_markdown("<thinking></thinking>rest"), "rest");
        // No tags
        assert_eq!(thinking_to_markdown("plain text"), "plain text");
        // Tag NOT at start - no change
        assert_eq!(
            thinking_to_markdown("text <thinking>inner</thinking> more"),
            "text <thinking>inner</thinking> more"
        );
    }

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
                content: vec![ContentBlock::Text(r#"[{"text": " status", "confidence": 0.9}]"#.to_string())],
                stop_reason: StopReason::EndTurn,
                model: "mock".to_string(),
                usage: None,
            })
        }

        fn name(&self) -> &str {
            "mock_delayed"
        }

        fn model_name(&self) -> &str {
            "mock-model"
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
    fn test_parse_completion_suggestions_plaintext_rejected() {
        // Issue #586: only JSON arrays in the documented shapes are accepted.
        // Plaintext (single word, JSON object, etc.) is treated as malformed.
        assert!(parse_completion_suggestions("status").unwrap().is_empty());
        assert!(parse_completion_suggestions("\"status\"").unwrap().is_empty());
        let err_obj = r#"{"error": "No output provided by user, no command to complete.", "command": "exit"}"#;
        assert!(parse_completion_suggestions(err_obj).unwrap().is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_empty_input() {
        assert!(parse_completion_suggestions("").unwrap().is_empty());
        assert!(parse_completion_suggestions("   ").unwrap().is_empty());
    }

    #[test]
    fn test_parse_completion_suggestions_code_fenced() {
        // Issue #548: some models wrap the JSON in ```json...``` fences
        let input = "```json\n[\"cd workspace/\", \"cd workspace/omnish\"]```";
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "cd workspace/");
        assert_eq!(result[1].text, "cd workspace/omnish");
    }

    #[test]
    fn test_parse_completion_suggestions_multiple_arrays_with_prose() {
        // Issue #571: the model emits a valid first array, then prose, then a
        // second array. The old find('[') + rfind(']') spanned both arrays,
        // producing invalid JSON and dumping the entire blob into the fallback
        // as a single suggestion. We should take only the first array.
        let input = "[\"git push\", \"git status\"]\n\nBased on the pattern of `git commit`, the next step is to push.\n\n[\"git push\"]";
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "git push");
        assert_eq!(result[1].text, "git status");
    }

    #[test]
    fn test_parse_completion_suggestions_bracket_inside_string() {
        // A `]` inside a JSON string must not be treated as the array terminator.
        let input = r#"["git commit -m \"do something with ]\"", "git status"]"#;
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "git commit -m \"do something with ]\"");
        assert_eq!(result[1].text, "git status");
    }

    #[test]
    fn test_parse_completion_suggestions_multiline_prose_rejected() {
        // Prose without any JSON array must not become a suggestion.
        let input = "I cannot suggest a completion here.\nPlease try again.";
        let result = parse_completion_suggestions(input).unwrap();
        assert!(result.is_empty());
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
        let backend = Arc::new(MultiBackend::from_single(Arc::new(MockDelayedBackend::new(100))));

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
        let backend = Arc::new(MultiBackend::from_single(Arc::new(MockDelayedBackend::new(100))));

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
