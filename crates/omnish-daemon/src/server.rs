use anyhow::Result;
use omnish_daemon::conversation_mgr::{ConversationManager, ThreadMeta};
use omnish_daemon::plugin::{PluginManager, PluginType};
use omnish_daemon::session_mgr::SessionManager;
use omnish_daemon::task_mgr::TaskManager;
use omnish_llm::backend::{ContentBlock, LlmBackend, LlmRequest, StopReason, TriggerType, UseCase};
use omnish_protocol::message::*;
use omnish_transport::rpc_server::RpcServer;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsAcceptor;

/// Load chat system prompt: base from embedded JSON, with optional user overrides
/// from ~/.omnish/prompts/chat.override.json (same fragment format — matching names replace).
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

/// Cached state for a paused agent loop awaiting a client-side tool result.
struct AgentLoopState {
    llm_req: LlmRequest,
    prior_len: usize,
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
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub sandbox_rules: SandboxRules,
    pub config_path: std::path::PathBuf,
    pub daemon_config: std::sync::Arc<std::sync::RwLock<omnish_common::config::DaemonConfig>>,
}

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
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
    chat_model_name: Option<String>,
    tool_params: HashMap<String, HashMap<String, serde_json::Value>>,
    opts: Arc<ServerOpts>,
}

/// Shallow-merge params into a JSON object. Source keys overwrite target keys.
fn merge_tool_params(target: &mut serde_json::Value, params: &HashMap<String, serde_json::Value>) {
    if let Some(obj) = target.as_object_mut() {
        for (k, v) in params {
            obj.insert(k.clone(), v.clone());
        }
    }
}

/// Execute a daemon-side external plugin by spawning a subprocess.
/// Uses the same stdin/stdout JSON protocol as client-side plugins.
async fn execute_daemon_plugin(
    executable: &std::path::Path,
    tool_name: &str,
    input: &serde_json::Value,
    proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> omnish_llm::tool::ToolResult {
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
            return omnish_llm::tool::ToolResult {
                tool_use_id: String::new(),
                content: format!("Failed to spawn plugin '{}': {}", executable.display(), e),
                is_error: true,
            };
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
                return omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Plugin exited with {}: {}", output.status, stderr.trim()),
                    is_error: true,
                };
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            #[derive(serde::Deserialize)]
            struct PluginResponse {
                content: String,
                #[serde(default)]
                is_error: bool,
            }
            match serde_json::from_str::<PluginResponse>(stdout.trim()) {
                Ok(resp) => omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: resp.content,
                    is_error: resp.is_error,
                },
                Err(e) => omnish_llm::tool::ToolResult {
                    tool_use_id: String::new(),
                    content: format!("Invalid plugin response: {e}"),
                    is_error: true,
                },
            }
        }
        Ok(Err(e)) => omnish_llm::tool::ToolResult {
            tool_use_id: String::new(),
            content: format!("Plugin I/O error: {e}"),
            is_error: true,
        },
        Err(_) => {
            // Timeout: the future was dropped, which drops the Child,
            // killing the process. Return error.
            omnish_llm::tool::ToolResult {
                tool_use_id: String::new(),
                content: "Plugin timed out (600s)".to_string(),
                is_error: true,
            }
        }
    }
}

impl DaemonServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_mgr: Arc<SessionManager>,
        llm_backend: Option<Arc<dyn LlmBackend>>,
        task_mgr: Arc<Mutex<TaskManager>>,
        conv_mgr: Arc<ConversationManager>,
        plugin_mgr: Arc<PluginManager>,
        tool_registry: Arc<omnish_daemon::tool_registry::ToolRegistry>,
        chat_model_name: Option<String>,
        tool_params: HashMap<String, HashMap<String, serde_json::Value>>,
        opts: Arc<ServerOpts>,
        formatter_mgr: Arc<omnish_daemon::formatter_mgr::FormatterManager>,
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
            chat_model_name,
            tool_params,
            opts,
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

        let mgr = self.session_mgr.clone();
        let llm = self.llm_backend.clone();
        let task_mgr = self.task_mgr.clone();
        let conv_mgr = self.conv_mgr.clone();
        let plugin_mgr = self.plugin_mgr.clone();
        let tool_registry = self.tool_registry.clone();
        let formatter_mgr = self.formatter_mgr.clone();
        let pending_loops = self.pending_agent_loops.clone();
        let cancel_flags = self.cancel_flags.clone();
        let active_threads = self.active_threads.clone();
        let chat_model_name = self.chat_model_name.clone();
        let tool_params = Arc::new(self.tool_params.clone());
        let opts = self.opts.clone();

        // Periodically sweep stale pending agent loop entries
        let pending_cleanup = self.pending_agent_loops.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut map = pending_cleanup.lock().await;
                map.retain(|req_id, state| {
                    if state.start.elapsed() > std::time::Duration::from_secs(120) {
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

        let io_requests_handler = io_requests.clone();
        let io_bytes_handler = io_bytes.clone();
        server
            .serve(
                move |msg, tx| {
                    let mgr = mgr.clone();
                    let llm = llm.clone();
                    let task_mgr = task_mgr.clone();
                    let conv_mgr = conv_mgr.clone();
                    let plugin_mgr = plugin_mgr.clone();
                    let tool_registry = tool_registry.clone();
                    let formatter_mgr = formatter_mgr.clone();
                    let pending_loops = pending_loops.clone();
                    let cancel_flags = cancel_flags.clone();
                    let active_threads = active_threads.clone();
                    let chat_model_name = chat_model_name.clone();
                    let tool_params = tool_params.clone();
                    let opts = opts.clone();
                    let io_requests = io_requests_handler.clone();
                    let io_bytes = io_bytes_handler.clone();
                    Box::pin(async move { handle_message(msg, mgr, &llm, &task_mgr, &conv_mgr, &plugin_mgr, &tool_registry, &formatter_mgr, &pending_loops, &cancel_flags, &active_threads, &chat_model_name, &tool_params, &opts, &io_requests, &io_bytes, tx).await })
                },
                Some(auth_token),
                tls_acceptor,
            )
            .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    msg: Message,
    mgr: Arc<SessionManager>,
    llm: &Option<Arc<dyn LlmBackend>>,
    task_mgr: &Arc<Mutex<TaskManager>>,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    tool_registry: &Arc<omnish_daemon::tool_registry::ToolRegistry>,
    formatter_mgr: &Arc<omnish_daemon::formatter_mgr::FormatterManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
    cancel_flags: &CancelFlags,
    active_threads: &ActiveThreads,
    chat_model_name: &Option<String>,
    tool_params: &Arc<HashMap<String, HashMap<String, serde_json::Value>>>,
    opts: &Arc<ServerOpts>,
    io_requests: &Arc<AtomicU64>,
    io_bytes: &Arc<AtomicU64>,
    tx: mpsc::Sender<Message>,
) {
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
            let _ = tx.send(Message::Ack).await;
        }
        Message::SessionEnd(s) => {
            if let Err(e) = mgr.end_session(&s.session_id).await {
                tracing::error!("end_session error: {}", e);
            }
            // Release any threads held by this session
            active_threads.lock().await.retain(|_, c| c.session_id != s.session_id);
            let _ = tx.send(Message::Ack).await;
        }
        Message::SessionUpdate(su) => {
            if let Err(e) = mgr.update_attrs(&su.session_id, su.timestamp_ms, su.attrs).await {
                tracing::error!("update_attrs error: {}", e);
            }
            let _ = tx.send(Message::Ack).await;
        }
        Message::IoData(io) => {
            io_requests.fetch_add(1, Ordering::Relaxed);
            io_bytes.fetch_add(io.data.len() as u64, Ordering::Relaxed);
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
            if llm.is_some() {
                let mgr = mgr_arc.clone();
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
                let result = handle_builtin_command(&req, mgr, task_mgr, llm, conv_mgr, tool_registry, formatter_mgr, active_threads).await;
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

            let content = if let Some(ref backend) = llm {
                match handle_llm_request(&req, mgr, backend).await {
                    Ok(response) => response.text(),
                    Err(e) => {
                        tracing::error!("LLM request failed: {}", e);
                        format!("Error: {}", e)
                    }
                }
            } else {
                "(LLM backend not configured)".to_string()
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
            let trimmed = req.input.trim();
            tracing::debug!("Checking omnish_debug: input={:?}, trimmed={:?}", req.input, trimmed);
            if trimmed == "omnish_debug" || trimmed.starts_with("omnish_debug ") {
                tracing::info!("omnish_debug matched, returning canned suggestions");
                let _ = tx.send(Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
                    sequence_id: req.sequence_id,
                    suggestions: vec![
                        omnish_protocol::message::CompletionSuggestion {
                            text: "omnish_debug yes".to_string(),
                            confidence: 1.0,
                        },
                        omnish_protocol::message::CompletionSuggestion {
                            text: "omnish_debug || echo works".to_string(),
                            confidence: 0.9,
                        },
                    ],
                })).await;
                return;
            }
            let reply = if let Some(ref backend) = llm {
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
                // Resume specific thread
                tracing::debug!("[ChatStart] resuming thread={}", tid);
                let raw_msgs = conv_mgr.load_raw_messages(tid);
                tracing::debug!("[ChatStart] loaded {} raw messages for thread={}", raw_msgs.len(), tid);
                if raw_msgs.is_empty() {
                    tracing::debug!("[ChatStart] thread not found, returning error");
                    Message::ChatReady(ChatReady {
                        request_id: cs.request_id,
                        thread_id: String::new(),
                        last_exchange: None,
                        earlier_count: 0,
                        model_name: chat_model_name.clone(),
                        history: None,
                        thread_host: None,
                        thread_cwd: None,
                        thread_summary: None,
                        error: Some("not_found".to_string()),
                        error_display: Some("Conversation not found".to_string()),
                    })
                } else if let Err(owner) = try_claim_thread(active_threads, tid, &cs.session_id).await {
                    tracing::debug!("[ChatStart] thread locked by session={}", owner);
                    let err = thread_locked_error(mgr, &owner).await;
                    Message::ChatReady(ChatReady {
                        request_id: cs.request_id,
                        thread_id: String::new(),
                        last_exchange: None,
                        earlier_count: 0,
                        model_name: chat_model_name.clone(),
                        history: None,
                        thread_host: None,
                        thread_cwd: None,
                        thread_summary: None,
                        error: Some("thread_locked".to_string()),
                        error_display: err.get("display").and_then(|d| d.as_str()).map(String::from),
                    })
                } else {
                    tracing::debug!("[ChatStart] claimed thread={}, reconstructing history", tid);
                    let old_meta = conv_mgr.load_meta(tid);
                    let merged_meta = ThreadMeta {
                        host: meta.host.clone(),
                        cwd: meta.cwd.clone(),
                        ..old_meta.clone()
                    };
                    if let Some(claim) = active_threads.lock().await.get_mut(tid) {
                        claim.pending_meta = Some(merged_meta);
                    }
                    let history_vals = reconstruct_history(&raw_msgs, tool_registry, formatter_mgr).await;
                    tracing::debug!("[ChatStart] history reconstructed: {} entries", history_vals.len());
                    let history: Vec<String> = history_vals.iter()
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                        .collect();
                    let thread_model = old_meta.model.and_then(|model_name| {
                        let is_default = llm.as_ref()
                            .map(|b| b.chat_default_name() == model_name)
                            .unwrap_or(true);
                        if is_default { None } else { Some(model_name) }
                    });
                    Message::ChatReady(ChatReady {
                        request_id: cs.request_id,
                        thread_id: tid.clone(),
                        last_exchange: None,
                        earlier_count: 0,
                        model_name: thread_model.or_else(|| chat_model_name.clone()),
                        history: Some(history),
                        thread_host: old_meta.host,
                        thread_cwd: old_meta.cwd,
                        thread_summary: old_meta.summary,
                        error: None,
                        error_display: None,
                    })
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
                })
            } else {
                // Resume latest thread
                tracing::debug!("[ChatStart] resuming latest thread");
                match conv_mgr.get_latest_thread() {
                    Some(tid) => {
                        tracing::debug!("[ChatStart] latest thread={}", tid);
                        if let Err(owner) = try_claim_thread(active_threads, &tid, &cs.session_id).await {
                            tracing::debug!("[ChatStart] latest thread locked by session={}", owner);
                            let err = thread_locked_error(mgr, &owner).await;
                            Message::ChatReady(ChatReady {
                                request_id: cs.request_id,
                                thread_id: String::new(),
                                last_exchange: None,
                                earlier_count: 0,
                                model_name: chat_model_name.clone(),
                                history: None,
                                thread_host: None,
                                thread_cwd: None,
                                thread_summary: None,
                                error: Some("thread_locked".to_string()),
                                error_display: err.get("display").and_then(|d| d.as_str()).map(String::from),
                            })
                        } else {
                            let old_meta = conv_mgr.load_meta(&tid);
                            let merged_meta = ThreadMeta {
                                host: meta.host.clone(),
                                cwd: meta.cwd.clone(),
                                ..old_meta.clone()
                            };
                            if let Some(claim) = active_threads.lock().await.get_mut(tid.as_str()) {
                                claim.pending_meta = Some(merged_meta);
                            }
                            let raw_msgs = conv_mgr.load_raw_messages(&tid);
                            let history_vals = reconstruct_history(&raw_msgs, tool_registry, formatter_mgr).await;
                            let history: Vec<String> = history_vals.iter()
                                .map(|v| serde_json::to_string(v).unwrap_or_default())
                                .collect();
                            let thread_model = old_meta.model.and_then(|model_name| {
                                let is_default = llm.as_ref()
                                    .map(|b| b.chat_default_name() == model_name)
                                    .unwrap_or(true);
                                if is_default { None } else { Some(model_name) }
                            });
                            Message::ChatReady(ChatReady {
                                request_id: cs.request_id,
                                thread_id: tid,
                                last_exchange: None,
                                earlier_count: 0,
                                model_name: thread_model.or_else(|| chat_model_name.clone()),
                                history: Some(history),
                                thread_host: old_meta.host,
                                thread_cwd: old_meta.cwd,
                                thread_summary: old_meta.summary,
                                error: None,
                                error_display: None,
                            })
                        }
                    }
                    None => {
                        tracing::debug!("[ChatStart] no threads found");
                        Message::ChatReady(ChatReady {
                            request_id: cs.request_id,
                            thread_id: String::new(),
                            last_exchange: None,
                            earlier_count: 0,
                            model_name: chat_model_name.clone(),
                            history: None,
                            thread_host: None,
                            thread_cwd: None,
                            thread_summary: None,
                            error: None,
                            error_display: None,
                        })
                    }
                }
            };
            let _ = tx.send(ready).await;
        }
        Message::ChatEnd(ce) => {
            tracing::debug!("[ChatEnd] session={} thread={}", ce.session_id, ce.thread_id);
            // Release thread binding
            let mut threads = active_threads.lock().await;
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
            touch_thread(active_threads, &cm.thread_id, conv_mgr).await;
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let req_id = cm.request_id.clone();
            cancel_flags.lock().await.insert(req_id.clone(), flag.clone());
            handle_chat_message(cm, mgr, llm, conv_mgr, plugin_mgr, tool_registry, formatter_mgr, pending_loops, tool_params, opts, tx, &flag).await;
            cancel_flags.lock().await.remove(&req_id);
        }
        Message::ChatToolResult(tr) => {
            touch_thread(active_threads, &tr.thread_id, conv_mgr).await;
            handle_tool_result(tr, mgr, conv_mgr, plugin_mgr, tool_registry, formatter_mgr, pending_loops, cancel_flags, tool_params, opts, tx).await;
        }
        Message::ChatInterrupt(ci) => {
            // Clean up pending agent loop and store partial results
            let state = if !ci.request_id.is_empty() {
                pending_loops.lock().await.remove(&ci.request_id)
            } else {
                None
            };

            if let Some(state) = state {
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

                // Store: prior messages + assistant tool_use + tool_results
                let mut to_store = state.llm_req.extra_messages[state.prior_len..].to_vec();
                // Replace first message (user+system-reminder) with clean user query
                to_store[0] = serde_json::json!({"role": "user", "content": ci.query});
                // Append tool results
                to_store.push(serde_json::json!({
                    "role": "user",
                    "content": result_content,
                }));
                conv_mgr.append_messages(&ci.thread_id, &to_store);
            } else if let Some(flag) = cancel_flags.lock().await.get(&ci.request_id) {
                // Agent loop is running daemon-side tools — signal it to stop.
                // The loop will store partial state to conversation when it detects the flag.
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            } else {
                // Loop already finished — just record the interrupt
                conv_mgr.append_messages(&ci.thread_id, &[
                    serde_json::json!({"role": "user", "content": ci.query}),
                    serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
                ]);
            }

            tracing::info!("Chat interrupted by user (thread={}, request={})", ci.thread_id, ci.request_id);
            let _ = tx.send(Message::Ack).await;
        }
        Message::ConfigQuery => {
            let config = opts.daemon_config.read().unwrap().clone();
            let (items, handlers) = crate::config_schema::build_config_items(&config);
            let _ = tx.send(Message::ConfigResponse { items, handlers }).await;
        }
        Message::ConfigUpdate { changes } => {
            let result = crate::config_schema::apply_config_changes(&opts.config_path, &changes);
            match result {
                Ok(()) => {
                    // Reload config after successful write
                    if let Ok(new_config) = omnish_common::config::load_daemon_config() {
                        *opts.daemon_config.write().unwrap() = new_config;
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
        Message::ConfigResponse { .. } | Message::ConfigUpdateResult { .. } => {
            // These are daemon→client messages, ignore if received
            let _ = tx.send(Message::Ack).await;
        }
        _ => {
            let _ = tx.send(Message::Ack).await;
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
        ContentBlock::Thinking(t) => serde_json::json!({"type": "thinking", "thinking": t}),
        ContentBlock::Text(t) => serde_json::json!({"type": "text", "text": t}),
        ContentBlock::ToolUse(tc) => serde_json::json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.input,
        }),
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

#[allow(clippy::too_many_arguments)]
async fn handle_chat_message(
    cm: ChatMessage,
    mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    tool_registry: &Arc<omnish_daemon::tool_registry::ToolRegistry>,
    formatter_mgr: &Arc<omnish_daemon::formatter_mgr::FormatterManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
    tool_params: &Arc<HashMap<String, HashMap<String, serde_json::Value>>>,
    opts: &Arc<ServerOpts>,
    tx: mpsc::Sender<Message>,
    cancel_flag: &Arc<std::sync::atomic::AtomicBool>,
) {
    if llm.is_none() {
        let _ = tx.send(Message::ChatResponse(ChatResponse {
            request_id: cm.request_id,
            thread_id: cm.thread_id,
            content: "(LLM backend not configured)".to_string(),
        })).await;
        return;
    }
    let backend = llm.as_ref().unwrap();

    // Handle model override
    if let Some(ref model_name) = cm.model {
        let mut meta = conv_mgr.load_meta(&cm.thread_id);
        meta.model = Some(model_name.clone());
        conv_mgr.save_meta(&cm.thread_id, &meta);
    }

    // Model-only message (no query) — just acknowledge
    if cm.query.is_empty() {
        let _ = tx.send(Message::Ack).await;
        return;
    }

    // Resolve per-thread model override for backend selection
    let meta = conv_mgr.load_meta(&cm.thread_id);
    let effective_backend: Arc<dyn LlmBackend> = meta.model.as_ref()
        .and_then(|name| backend.get_backend_by_name(name))
        .unwrap_or_else(|| backend.clone());

    let use_case = UseCase::Chat;
    let max_context_chars = effective_backend.max_content_chars_for_use_case(use_case);

    let ChatSetup { command_query_tool, tools, system_prompt } =
        build_chat_setup(mgr, tool_registry).await;

    // Get session attrs from client probes (cwd, platform, os_version, etc.)
    let session_attrs = mgr.get_session_attrs(&cm.session_id).await;

    // Build system-reminder with time, cwd, and last 5 commands
    let reminder = command_query_tool.build_system_reminder(5, &session_attrs);

    // Load prior conversation history as raw JSON
    let mut extra_messages = conv_mgr.load_raw_messages(&cm.thread_id);
    let prior_len = extra_messages.len();

    // User message for LLM: includes system-reminder
    let llm_user_content = format!("{}\n\n{}", cm.query, reminder);
    extra_messages.push(serde_json::json!({"role": "user", "content": llm_user_content}));

    let llm_req = LlmRequest {
        context: String::new(),
        query: None,
        trigger: TriggerType::Manual,
        session_ids: vec![cm.session_id.clone()],
        use_case,
        max_content_chars: max_context_chars,
        conversation: vec![],
        system_prompt: Some(system_prompt),
        enable_thinking: Some(true), // Enable thinking mode for chat
        tools,
        extra_messages,
    };

    let state = AgentLoopState {
        llm_req,
        prior_len,
        pending_tool_calls: vec![],
        completed_results: vec![],
        iteration: 0,
        cm,
        start: std::time::Instant::now(),
        command_query_tool,
        effective_backend,
        llm_retries: 0,
    };

    run_agent_loop(state, conv_mgr, plugin_mgr, tool_registry, formatter_mgr, pending_loops, tool_params, opts, tx, cancel_flag).await;
}

/// Handle a ChatToolResult from the client — accumulate results, resume when all are received.
#[allow(clippy::too_many_arguments)]
async fn handle_tool_result(
    tr: ChatToolResult,
    _mgr: &SessionManager,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    tool_registry: &Arc<omnish_daemon::tool_registry::ToolRegistry>,
    formatter_mgr: &Arc<omnish_daemon::formatter_mgr::FormatterManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
    cancel_flags: &CancelFlags,
    tool_params: &Arc<HashMap<String, HashMap<String, serde_json::Value>>>,
    opts: &Arc<ServerOpts>,
    tx: mpsc::Sender<Message>,
) {
    let mut map = pending_loops.lock().await;
    let state = match map.get_mut(&tr.request_id) {
        Some(s) => s,
        None => {
            tracing::warn!("No pending agent loop for request_id={}", tr.request_id);
            let _ = tx.send(Message::Ack).await;
            return;
        }
    };

    // Check if the agent loop has timed out (10 minutes — bash commands like builds can be slow)
    if state.start.elapsed() > std::time::Duration::from_secs(600) {
        map.remove(&tr.request_id);
        drop(map);
        tracing::warn!("Agent loop timed out for request_id={}", tr.request_id);
        let _ = tx.send(Message::ChatResponse(ChatResponse {
            request_id: tr.request_id,
            thread_id: tr.thread_id,
            content: "Error: client-side tool execution timed out".to_string(),
        })).await;
        return;
    }

    // Add the received client-side tool result
    let tool_call_id = tr.tool_call_id.clone();
    state.completed_results.push(omnish_llm::tool::ToolResult {
        tool_use_id: tr.tool_call_id.clone(),
        content: tr.content,
        is_error: tr.is_error,
    });

    // Generate immediate ChatToolStatus for this result
    if let Some(result) = state.completed_results.iter().find(|r| r.tool_use_id == tool_call_id) {
        if let Some(tc) = state.pending_tool_calls.iter().find(|tc| tc.id == tool_call_id) {
            let formatter_name = tool_registry.formatter_name(&tc.name);
            let display_name = tool_registry.display_name(&tc.name).to_string();
            let fmt_out = formatter_mgr.format(formatter_name, &omnish_plugin::formatter::FormatInput {
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
        // More results expected — keep waiting (status already sent via tx)
        return;
    }

    // All tool calls complete — remove state and continue agent loop
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
    state.llm_req.extra_messages.push(serde_json::json!({
        "role": "user",
        "content": result_content,
    }));

    // Clear pending state for next iteration
    state.pending_tool_calls.clear();
    state.completed_results.clear();
    state.iteration += 1;

    // Continue agent loop — register cancel flag so ChatInterrupt can signal it
    let req_id = state.cm.request_id.clone();
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    cancel_flags.lock().await.insert(req_id.clone(), flag.clone());
    run_agent_loop(state, conv_mgr, plugin_mgr, tool_registry, formatter_mgr, pending_loops, tool_params, opts, tx, &flag).await;
    cancel_flags.lock().await.remove(&req_id);
}

/// Core agent loop: calls LLM, executes tools, pauses on client-side tools.
/// Used by both `handle_chat_message` (initial) and `handle_tool_result` (resumption).
/// Messages are sent incrementally through `tx` as they're produced (streaming).
#[allow(clippy::too_many_arguments)]
async fn run_agent_loop(
    mut state: AgentLoopState,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    tool_registry: &Arc<omnish_daemon::tool_registry::ToolRegistry>,
    formatter_mgr: &Arc<omnish_daemon::formatter_mgr::FormatterManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
    tool_params: &Arc<HashMap<String, HashMap<String, serde_json::Value>>>,
    opts: &Arc<ServerOpts>,
    tx: mpsc::Sender<Message>,
    cancel_flag: &Arc<std::sync::atomic::AtomicBool>,
) {
    let backend = &state.effective_backend;

    let max_iterations = 100;

    for iteration in state.iteration..max_iterations {
        // Check if user interrupted (Ctrl+C)
        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("Agent loop cancelled by user at iteration {} (thread={})", iteration, state.cm.thread_id);
            // Store partial state: everything from this exchange so far
            let mut to_store = state.llm_req.extra_messages[state.prior_len..].to_vec();
            if !to_store.is_empty() {
                to_store[0] = serde_json::json!({"role": "user", "content": state.cm.query});
            }
            // Append event marker so LLM knows the user interrupted
            to_store.push(serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}));
            conv_mgr.append_messages(&state.cm.thread_id, &to_store);
            return;
        }

        match backend.complete(&state.llm_req).await {
            Ok(response) => {
                state.llm_retries = 0;
                if response.stop_reason == StopReason::ToolUse {
                    let tool_calls = response.tool_calls();
                    if tool_calls.is_empty() {
                        break;
                    }

                    // Build assistant message preserving original block order
                    // (thinking, text, tool_use — order matters for DeepSeek-compatible APIs)
                    let assistant_content: Vec<serde_json::Value> = response
                        .content
                        .iter()
                        .map(content_block_to_json)
                        .collect();
                    state.llm_req.extra_messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": assistant_content,
                    }));

                    // Send LLM's text blocks to client immediately
                    for block in &response.content {
                        if let ContentBlock::Text(text) = block {
                            if !text.trim().is_empty()
                                && tx.send(Message::ChatToolStatus(ChatToolStatus {
                                    request_id: state.cm.request_id.clone(),
                                    thread_id: state.cm.thread_id.clone(),
                                    tool_name: String::new(),
                                    status: text.clone(),
                                    tool_call_id: None,
                                    status_icon: None,
                                    display_name: None,
                                    param_desc: None,
                                    result_compact: None,
                                    result_full: None,
                                })).await.is_err() { return; }
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
                        })).await.is_err() { return; }

                        let ptype = tool_registry.plugin_type(&tc.name);
                        if ptype == Some(PluginType::ClientTool) {
                            // Client-side tool: forward to client for parallel execution
                            let mut merged_input = tc.input.clone();
                            if let Some(override_params) = tool_registry.override_params(&tc.name) {
                                merge_tool_params(&mut merged_input, &override_params);
                            }
                            if let Some(config_params) = tool_params.get(&tc.name) {
                                merge_tool_params(&mut merged_input, config_params);
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
                            if tx.send(Message::ChatToolCall(ChatToolCall {
                                request_id: state.cm.request_id.clone(),
                                thread_id: state.cm.thread_id.clone(),
                                tool_name: tc.name.clone(),
                                tool_call_id: tc.id.clone(),
                                input: serde_json::to_string(&merged_input).unwrap_or_default(),
                                plugin_name: tool_registry.plugin_name(&tc.name).unwrap_or("builtin").to_string(),
                                sandboxed: matched_rule.is_none(),
                            })).await.is_err() { return; }
                            has_client_tools = true;
                        } else {
                            // Daemon-side tool: execute directly
                            let mut merged_input = tc.input.clone();
                            if let Some(override_params) = tool_registry.override_params(&tc.name) {
                                merge_tool_params(&mut merged_input, &override_params);
                            }
                            if let Some(config_params) = tool_params.get(&tc.name) {
                                merge_tool_params(&mut merged_input, config_params);
                            }

                            let mut result = if tool_registry.plugin_type(&tc.name).is_some() {
                                if let Some(exe) = plugin_mgr.plugin_executable(&tc.name) {
                                    execute_daemon_plugin(&exe, &tc.name, &merged_input, opts.proxy.as_deref(), opts.no_proxy.as_deref()).await
                                } else {
                                    omnish_llm::tool::ToolResult {
                                        tool_use_id: String::new(),
                                        content: format!("Unknown daemon tool: {}", tc.name),
                                        is_error: true,
                                    }
                                }
                            } else if tool_registry.is_known(&tc.name) {
                                state.command_query_tool.execute(&tc.name, &merged_input)
                            } else {
                                omnish_llm::tool::ToolResult {
                                    tool_use_id: String::new(),
                                    content: format!("Unknown tool: {}", tc.name),
                                    is_error: true,
                                }
                            };
                            result.tool_use_id = tc.id.clone();

                            // Post-execution: send update ChatToolStatus with formatted results immediately
                            let post_display = tool_registry.display_name(&tc.name).to_string();
                            let post_out = formatter_mgr.format(tool_registry.formatter_name(&tc.name), &omnish_plugin::formatter::FormatInput {
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
                            })).await.is_err() { return; }

                            tool_results.push(result);
                        }
                    }

                    if cancelled {
                        // Cancelled mid-tool-execution — store partial state
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
                        state.llm_req.extra_messages.push(serde_json::json!({
                            "role": "user",
                            "content": result_content,
                        }));
                        let mut to_store = state.llm_req.extra_messages[state.prior_len..].to_vec();
                        if !to_store.is_empty() {
                            to_store[0] = serde_json::json!({"role": "user", "content": state.cm.query});
                        }
                        to_store.push(serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}));
                        conv_mgr.append_messages(&state.cm.thread_id, &to_store);
                        return;
                    }

                    if has_client_tools {
                        // Pause loop — client will execute tools in parallel and send results back
                        state.pending_tool_calls = tool_calls.iter().map(|tc| (*tc).clone()).collect();
                        state.completed_results = tool_results;
                        state.iteration = iteration;
                        let request_id = state.cm.request_id.clone();
                        pending_loops.lock().await.insert(request_id, state);
                        return; // tx dropped → Ack sent by spawn_connection
                    }

                    // All tools were daemon-side — build tool_result and continue
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
                    state.llm_req.extra_messages.push(serde_json::json!({
                        "role": "user",
                        "content": result_content,
                    }));

                    continue;
                }

                // EndTurn or MaxTokens — extract final text and store
                let text = response.text();
                tracing::info!(
                    "Chat LLM completed in {:?} ({} tool iterations, thread={})",
                    state.start.elapsed(),
                    iteration,
                    state.cm.thread_id
                );
                // Push final assistant response preserving original block order
                let has_thinking = response.content.iter().any(|b| matches!(b, ContentBlock::Thinking(_)));
                let assistant_msg = if has_thinking {
                    let content: Vec<serde_json::Value> = response.content.iter()
                        .map(content_block_to_json)
                        .collect();
                    serde_json::json!({ "role": "assistant", "content": content })
                } else {
                    serde_json::json!({ "role": "assistant", "content": text })
                };
                state.llm_req.extra_messages.push(assistant_msg);
                // Store new messages without system-reminder in user message
                let mut to_store = state.llm_req.extra_messages[state.prior_len..].to_vec();
                to_store[0] = serde_json::json!({"role": "user", "content": state.cm.query});
                conv_mgr.append_messages(&state.cm.thread_id, &to_store);
                let _ = tx.send(Message::ChatResponse(ChatResponse {
                    request_id: state.cm.request_id.clone(),
                    thread_id: state.cm.thread_id.clone(),
                    content: text,
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
                        "LLM connection error (retry {}/2, thread={}): {} — retrying in {}s",
                        state.llm_retries, state.cm.thread_id, err_str, backoff.as_secs()
                    );
                    tokio::time::sleep(backoff).await;
                    continue;
                }

                tracing::error!("Chat LLM failed: {}", e);

                // Save progress so the conversation can be continued
                let has_progress = state.llm_req.extra_messages.len() > state.prior_len + 1;
                if has_progress {
                    let error_note = "Connection to the AI service was lost. Your previous tool results have been saved — you can continue by sending another message.";
                    state.llm_req.extra_messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": error_note,
                    }));
                    let mut to_store = state.llm_req.extra_messages[state.prior_len..].to_vec();
                    to_store[0] = serde_json::json!({"role": "user", "content": state.cm.query});
                    conv_mgr.append_messages(&state.cm.thread_id, &to_store);

                    let _ = tx.send(Message::ChatResponse(ChatResponse {
                        request_id: state.cm.request_id.clone(),
                        thread_id: state.cm.thread_id.clone(),
                        content: error_note.to_string(),
                    })).await;
                } else {
                    let user_msg = format!("Failed to reach the AI service: {}. Please try again.", err_str);
                    let _ = tx.send(Message::ChatResponse(ChatResponse {
                        request_id: state.cm.request_id.clone(),
                        thread_id: state.cm.thread_id.clone(),
                        content: user_msg,
                    })).await;
                }
                return;
            }
        }
    }

    // Exhausted iterations — store what we have
    tracing::warn!(
        "Agent loop exhausted {} iterations (thread={})",
        max_iterations,
        state.cm.thread_id
    );
    let text = "(Agent reached maximum tool call limit)".to_string();
    state.llm_req.extra_messages.push(serde_json::json!({
        "role": "assistant",
        "content": text,
    }));
    let mut to_store = state.llm_req.extra_messages[state.prior_len..].to_vec();
    to_store[0] = serde_json::json!({"role": "user", "content": state.cm.query});
    conv_mgr.append_messages(&state.cm.thread_id, &to_store);
    let _ = tx.send(Message::ChatResponse(ChatResponse {
        request_id: state.cm.request_id,
        thread_id: state.cm.thread_id,
        content: text,
    })).await;
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
        system_prompt: None,
        enable_thinking: Some(false), // Disable thinking for completion
        tools: vec![],
        extra_messages: vec![],
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
                                let fmt_out = formatter_mgr.format(tool_registry.formatter_name(&tool_name), &omnish_plugin::formatter::FormatInput {
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
    llm_backend: &Option<Arc<dyn LlmBackend>>,
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
        let is_default = llm_backend.as_ref()
            .map(|b| b.chat_default_name() == model_name)
            .unwrap_or(true);
        if !is_default {
            json["model"] = serde_json::json!(model_name);
        }
    }
    json
}

#[allow(clippy::too_many_arguments)]
async fn handle_builtin_command(req: &Request, mgr: &SessionManager, task_mgr: &Mutex<TaskManager>, llm_backend: &Option<Arc<dyn LlmBackend>>, conv_mgr: &Arc<ConversationManager>, tool_registry: &omnish_daemon::tool_registry::ToolRegistry, formatter_mgr: &omnish_daemon::formatter_mgr::FormatterManager, active_threads: &ActiveThreads) -> serde_json::Value {
    let sub = req.query.strip_prefix("__cmd:").unwrap_or("");

    // Build system-reminder for context display
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(commands, stream_reader);
    let session_attrs = mgr.get_session_attrs(&req.session_id).await;
    let reminder = command_query_tool.build_system_reminder(5, &session_attrs);

    // Handle /context chat:<thread_id> — show conversation context + system-reminder
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

    // Handle /context chat (without thread_id) — show only system-reminder
    if sub == "context chat" {
        return cmd_display(reminder);
    }

    // Handle /context <scenario> for showing context for different scenarios
    if let Some(scenario) = sub.strip_prefix("context ") {
        return cmd_display(handle_context_scenario(scenario, req, mgr, llm_backend).await);
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
    // Handle /conversations del <thread_id> — delete a conversation by thread ID
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
    // Handle /model — list available backends with selected flag
    if sub == "models" || sub.starts_with("models ") {
        let thread_id = sub.strip_prefix("models ").unwrap_or("").trim();

        if let Some(ref backend) = *llm_backend {
            let backends = backend.list_backends();
            if backends.is_empty() {
                return cmd_display("No LLM backends configured".to_string());
            }

            // Determine which backend is selected for this thread
            let selected_name = if !thread_id.is_empty() {
                let meta = conv_mgr.load_meta(thread_id);
                meta.model.unwrap_or_else(|| backend.chat_default_name().to_string())
            } else {
                backend.chat_default_name().to_string()
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
        } else {
            return cmd_display("No LLM backends configured".to_string());
        }
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
    match sub {
        "context" => {
            // Default to completion context (most common LLM use case)
            match mgr.build_completion_context(&req.session_id, None).await {
                Ok(ctx) => cmd_display(ctx),
                Err(e) => cmd_display(format!("Error: {}", e)),
            }
        }
        "sessions" => cmd_display(mgr.format_sessions_list(&req.session_id).await),
        "conversations" => format_conversations_json(conv_mgr, active_threads).await,
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
async fn format_conversations_json(conv_mgr: &Arc<ConversationManager>, active_threads: &ActiveThreads) -> serde_json::Value {
    let conversations = conv_mgr.list_conversations();
    if conversations.is_empty() {
        return cmd_display("No conversations yet. Start a chat with :");
    }

    let locked_set: std::collections::HashSet<String> = {
        let threads = active_threads.lock().await;
        threads.keys().cloned().collect()
    };

    let mut output = String::from("Conversations:\n");
    let mut thread_ids = Vec::new();
    let mut locked_threads = Vec::new();
    for (i, (thread_id, modified, exchange_count, last_question)) in conversations.into_iter().enumerate() {
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
    serde_json::json!({
        "display": output,
        "thread_ids": thread_ids,
        "locked_threads": locked_threads,
    })
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
async fn handle_context_scenario(scenario: &str, req: &Request, mgr: &SessionManager, llm_backend: &Option<Arc<dyn LlmBackend>>) -> String {
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
        system_prompt: None,
        enable_thinking: Some(false), // Disable thinking for completion requests
        tools: vec![],
        extra_messages: vec![],
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
            tracing::debug!("Completion LLM raw response: {:?}", response.text());

            // Log thinking length and content
            if let Some(ref thinking) = response.thinking() {
                tracing::info!("Completion LLM thinking: {} chars", thinking.len());
                tracing::debug!("Completion LLM thinking content: {}", thinking);
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
    use omnish_llm::backend::{ContentBlock, LlmBackend, LlmRequest, LlmResponse, StopReason};
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
                content: vec![ContentBlock::Text(r#"[{"text": " status", "confidence": 0.9}]"#.to_string())],
                stop_reason: StopReason::EndTurn,
                model: "mock".to_string(),
                usage: None,
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
