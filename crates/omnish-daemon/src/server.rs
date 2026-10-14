use anyhow::Result;
use omnish_daemon::conversation_mgr::{host_change_event, ConversationManager, ThreadMeta};
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
    /// Last time this loop made progress: refreshed when it is (re)parked to wait
    /// for client-side tools and when each tool result arrives. The stale-loop
    /// sweep measures idle time from here (not `start`), so a turn whose tools run
    /// for a long time but keep returning results is never reclaimed mid-flight.
    last_active: std::time::Instant,
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
    /// Thread generation captured when this loop was started/resumed. Each
    /// new ChatMessage on the thread bumps the counter; if the loop's saved
    /// generation no longer matches `thread_generations[thread_id]`, the
    /// loop is stale and must drop its in-flight work without writing to
    /// disk or sending response messages.
    generation: u64,
    /// Instant the current LLM call was sent. Used to report how long execution
    /// ran before a user interrupt while waiting on the LLM (issue #682).
    llm_call_started: Option<std::time::Instant>,
    /// Start time of each tool dispatched in this loop, keyed by tool_call_id.
    /// Used to report the earliest unfinished tool's runtime on interrupt.
    tool_started: HashMap<String, std::time::Instant>,
    /// Set when an auto compaction in this loop failed, so it is not retried
    /// before every remaining LLM call (issue #691).
    auto_compact_failed: bool,
}

impl AgentLoopState {
    /// How long execution ran before an interrupt, per issue #682: the earliest
    /// start among `unfinished_ids` (tools still awaiting a result), else the
    /// current LLM call, else the loop start.
    fn interrupt_elapsed<'a>(&self, unfinished_ids: impl Iterator<Item = &'a str>) -> std::time::Duration {
        interrupt_start(&self.tool_started, unfinished_ids, self.llm_call_started, self.start)
            .elapsed()
    }
}

/// Resolve the instant execution "started" for interrupt reporting: the earliest
/// unfinished tool, else the current LLM call, else the loop start.
fn interrupt_start<'a>(
    tool_started: &HashMap<String, std::time::Instant>,
    unfinished_ids: impl Iterator<Item = &'a str>,
    llm_call_started: Option<std::time::Instant>,
    loop_start: std::time::Instant,
) -> std::time::Instant {
    unfinished_ids
        .filter_map(|id| tool_started.get(id).copied())
        .min()
        .or(llm_call_started)
        .unwrap_or(loop_start)
}

/// Base idle window for a parked agent loop: reclaim its in-memory state only
/// after this long with no progress. Covers loops truly abandoned by a client
/// that vanished mid-tool. Measured from `AgentLoopState::last_active`.
const PARKED_LOOP_BASE_IDLE: std::time::Duration = std::time::Duration::from_secs(1800);

/// Expected max wall-clock for a single dispatched client tool, derived from the
/// tool's own `timeout` parameter so a legitimately long tool run is not reaped
/// before it can return. Returns ZERO for tools that declare no timeout.
fn tool_expected_duration(name: &str, input: &serde_json::Value) -> std::time::Duration {
    use std::time::Duration;
    match name {
        // bash: `timeout` is in milliseconds (default 5m, hard max 15m).
        "bash" => {
            let ms = input.get("timeout").and_then(|v| v.as_u64()).unwrap_or(300_000).min(900_000);
            Duration::from_millis(ms)
        }
        // grep: `timeout` is in seconds (default 60s, clamped to 300s).
        "grep" => {
            let secs = input.get("timeout").and_then(|v| v.as_u64()).unwrap_or(60).min(300);
            Duration::from_secs(secs)
        }
        _ => Duration::ZERO,
    }
}

/// Idle deadline for a parked loop: the base window, extended to cover the
/// longest-running pending client tool (plus a margin) so a tool that declares a
/// timeout beyond the base window still gets to finish before the sweep reclaims
/// the loop.
fn parked_loop_idle_deadline(pending: &[omnish_llm::tool::ToolCall]) -> std::time::Duration {
    let margin = std::time::Duration::from_secs(60);
    let max_tool = pending
        .iter()
        .map(|tc| tool_expected_duration(&tc.name, &tc.input))
        .max()
        .unwrap_or_default();
    PARKED_LOOP_BASE_IDLE.max(max_tool + margin)
}

/// The host/cwd snapshot captured at ChatStart, deferred until the first
/// ChatMessage so that cancelling the resume doesn't overwrite the thread's
/// stored host/cwd. Deliberately narrow (not a full `ThreadMeta`) so adding a
/// field here is a compile error rather than a silently dropped update.
struct PendingMeta {
    host: Option<String>,
    cwd: Option<String>,
}

/// Tracks which session is actively using each thread.
struct ThreadClaim {
    session_id: String,
    last_active: std::time::Instant,
    /// Meta to save when the first ChatMessage arrives (deferred from ChatStart
    /// so that cancelling the resume doesn't overwrite the thread's stored host/cwd).
    pending_meta: Option<PendingMeta>,
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
async fn touch_thread(
    active_threads: &ActiveThreads,
    thread_id: &str,
    conv_mgr: &ConversationManager,
    session_mgr: &SessionManager,
) {
    let (pending, session_id) = {
        let mut threads = active_threads.lock().await;
        match threads.get_mut(thread_id) {
            Some(claim) => {
                claim.last_active = std::time::Instant::now();
                (claim.pending_meta.take(), claim.session_id.clone())
            }
            None => return,
        }
    };
    if let Some(pending) = pending {
        // Record where the resumed conversation actually continues, using the
        // session's CURRENT host/cwd rather than the snapshot captured at
        // ChatStart. A resume that cd'd back to the thread's original directory
        // (CdToOld) or stayed put has already pushed the resolved cwd via
        // SessionUpdate before the first message, so this avoids overwriting the
        // thread's real cwd with the transient resume-time cwd (issue #672).
        // update_meta reloads from disk, preserving unrelated fields.
        let live_cwd = session_mgr.get_session_attr(&session_id, "shell_cwd").await;
        // Never persist a blank host: an empty probe would otherwise overwrite a
        // real host with Some("") and permanently defeat change detection.
        let live_host = session_mgr
            .get_session_attr(&session_id, "hostname")
            .await
            .filter(|s| !s.trim().is_empty());
        conv_mgr.update_meta(thread_id, |m| {
            // Compare the host we are about to overwrite against the value we
            // will actually persist (live probe first, resume snapshot as
            // fallback), then queue a one-shot `<event>` so the next message
            // tells the LLM the thread moved hosts (issue #679). Doing this here,
            // on the same value that is written, avoids missing the switch when
            // the hostname probe lands after ChatStart.
            let old_host = m.host.take();
            m.cwd = live_cwd.or(pending.cwd);
            let new_host =
                live_host.or_else(|| pending.host.filter(|s| !s.trim().is_empty()));
            match new_host {
                Some(new_host) => {
                    if let Some(event) =
                        host_change_event(old_host.as_deref(), Some(new_host.as_str()))
                    {
                        m.queue_event(&event);
                    }
                    m.host = Some(new_host);
                }
                // No usable probe/snapshot: keep the stored host, never clear it.
                None => m.host = old_host,
            }
        });
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

/// Per-thread monotonic counter. Bumped at the start of each ChatMessage so
/// any agent loop still in-flight on the same thread (e.g. waiting on an LLM
/// call that the user already abandoned) can detect supersession and discard
/// its work instead of dirtying the conversation file.
type ThreadGenerations = Arc<Mutex<HashMap<String, u64>>>;

/// Bump and return the new generation for `thread_id`.
async fn bump_generation(gens: &ThreadGenerations, thread_id: &str) -> u64 {
    let mut map = gens.lock().await;
    let entry = map.entry(thread_id.to_string()).or_insert(0);
    *entry = entry.wrapping_add(1);
    *entry
}

/// Read the current generation without mutating it.
async fn current_generation(gens: &ThreadGenerations, thread_id: &str) -> u64 {
    *gens.lock().await.get(thread_id).unwrap_or(&0)
}

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
    thread_generations: ThreadGenerations,
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
    /// Per-thread generation counters used to invalidate superseded agent loops.
    thread_generations: ThreadGenerations,
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

/// Parse `omnish_debug` arguments into (delay_ms, suffix_len).
///
/// Accepts keywords in any order plus the legacy bare-number form:
///   ""                       -> (None, None)
///   "60"                     -> (None, Some(60))         legacy bare N
///   "delay 500"              -> (Some(500), None)
///   "length 60"              -> (None, Some(60))
///   "delay 500 length 60"    -> (Some(500), Some(60))
///   "length 60 delay 500"    -> (Some(500), Some(60))
///
/// Unknown tokens are silently ignored so the parser stays forgiving in
/// test fixtures. The bare-number form is only consulted when no `length`
/// keyword has set `suffix_len` yet.
fn parse_omnish_debug_args(arg: &str) -> (Option<u64>, Option<usize>) {
    let mut delay_ms: Option<u64> = None;
    let mut suffix_len: Option<usize> = None;
    let mut iter = arg.split_whitespace();
    while let Some(tok) = iter.next() {
        match tok {
            "delay" => {
                if let Some(v) = iter.next().and_then(|s| s.parse::<u64>().ok()) {
                    delay_ms = Some(v);
                }
            }
            "length" | "len" => {
                if let Some(v) = iter.next().and_then(|s| s.parse::<usize>().ok()) {
                    suffix_len = Some(v);
                }
            }
            s if suffix_len.is_none() => {
                if let Ok(n) = s.parse::<usize>() {
                    suffix_len = Some(n);
                }
            }
            _ => {}
        }
    }
    (delay_ms, suffix_len)
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
    match omnish_daemon::llm_pool::complete(backend, &req).await {
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
        push_registry: PushRegistry,
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
            thread_generations: Arc::new(Mutex::new(HashMap::new())),
            active_threads: Arc::new(Mutex::new(HashMap::new())),
            opts,
            update_cache,
            plugin_bundler,
            push_registry,
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

        // Periodically reclaim parked agent-loop state whose owning client is
        // gone. A loop is reclaimed only when its session has NO live transport
        // connection AND has been idle past the deadline: while the client is
        // still connected it presumably keeps executing the parked tools, no
        // matter how long they legitimately run, so it is never reaped (#670).
        // Idle is measured from `last_active` (refreshed on park and on each
        // tool result); the deadline auto-extends to cover a pending tool's
        // declared timeout, still bounding a vanished client's state.
        let pending_cleanup = self.pending_agent_loops.clone();
        let sweep_sessions = self.session_mgr.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                // Snapshot candidates under the lock, then evaluate connectivity
                // without holding it (retain can't await).
                let candidates: Vec<(String, String, std::time::Duration, std::time::Duration)> = {
                    let map = pending_cleanup.lock().await;
                    map.iter()
                        .map(|(req_id, state)| (
                            req_id.clone(),
                            state.cm.session_id.clone(),
                            state.last_active.elapsed(),
                            parked_loop_idle_deadline(&state.pending_tool_calls),
                        ))
                        .collect()
                };
                let mut to_reclaim = Vec::new();
                for (req_id, session_id, idle, deadline) in candidates {
                    if idle <= deadline {
                        continue;
                    }
                    if sweep_sessions.is_session_connected(&session_id).await {
                        // Client still connected: keep the loop (issue #670).
                        continue;
                    }
                    to_reclaim.push((req_id, idle, deadline));
                }
                if !to_reclaim.is_empty() {
                    let mut map = pending_cleanup.lock().await;
                    for (req_id, idle, deadline) in to_reclaim {
                        // Re-check under the lock in case the loop was resumed or
                        // re-parked (fresh last_active) since the snapshot.
                        if let Some(state) = map.get(&req_id) {
                            if state.last_active.elapsed() > parked_loop_idle_deadline(&state.pending_tool_calls) {
                                tracing::warn!(
                                    "Reclaiming idle agent loop state: {} (session gone, idle {:?} > deadline {:?})",
                                    req_id, idle, deadline
                                );
                                map.remove(&req_id);
                            }
                        }
                    }
                }
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
            thread_generations: self.thread_generations.clone(),
            active_threads: self.active_threads.clone(),
            opts: self.opts.clone(),
            update_cache: self.update_cache.clone(),
            plugin_bundler: self.plugin_bundler.clone(),
            io_requests,
            io_bytes,
            push_registry: self.push_registry.clone(),
        });

        // Mark sessions whose transport connection just dropped as pending
        // end. The DisconnectSweepTask scheduled job does the actual
        // end_session after the grace window, so brief disconnects (daemon
        // restart, network blip) don't trip a false positive.
        let on_disconnect: omnish_transport::rpc_server::OnDisconnect = {
            let mgr = self.session_mgr.clone();
            Arc::new(move |conn_id: u64| {
                let mgr = mgr.clone();
                Box::pin(async move {
                    let _ = mgr.mark_conn_disconnected(conn_id).await;
                })
            })
        };

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
                Some(on_disconnect),
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
            let conn_id = omnish_transport::rpc_server::CONN_ID
                .try_with(|id| *id)
                .ok();
            if let Err(e) = mgr
                .register(&s.session_id, s.parent_session_id, s.attrs, conn_id)
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
                // A heartbeat for an unknown session is usually benign (the
                // session was already evicted/ended and its data is safe on
                // disk), but with the live-connection eviction guard it
                // should no longer happen for an active client. Keep it at
                // info so an unexpected eviction is still visible.
                tracing::info!("update_attrs skipped: {}", e);
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
            // Supports (keywords may be combined in any order):
            //   "omnish_debug"                          -> ghost " yes"
            //   "omnish_debug delay <ms>"               -> sleep <ms> then ghost " yes"
            //   "omnish_debug <N>"                      -> ghost of exact length N (issue #629)
            //   "omnish_debug length <N>"               -> same as bare N
            //   "omnish_debug delay <ms> length <N>"    -> sleep <ms> then N-char ghost (issue #635)
            let trimmed = req.input.trim();
            if trimmed == "omnish_debug" || trimmed.starts_with("omnish_debug ") {
                let arg = trimmed.strip_prefix("omnish_debug").map(str::trim).unwrap_or("");
                let (delay_ms, suffix_len) = parse_omnish_debug_args(arg);
                match (delay_ms, suffix_len) {
                    (Some(ms), Some(n)) => {
                        tracing::info!("omnish_debug delay {}ms len={}, seq={}", ms, n, req.sequence_id);
                        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    }
                    (Some(ms), None) => {
                        tracing::info!("omnish_debug delay {}ms, seq={}", ms, req.sequence_id);
                        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    }
                    (None, Some(n)) => {
                        tracing::info!("omnish_debug len={}, seq={}", n, req.sequence_id);
                    }
                    (None, None) => {
                        tracing::info!("omnish_debug matched, returning canned suggestions");
                    }
                }
                let primary_suffix = match suffix_len {
                    Some(n) if n > 0 => {
                        // Leading space + (N-1) 'x' chars = N-char ghost suffix.
                        let mut s = String::with_capacity(n);
                        s.push(' ');
                        s.extend(std::iter::repeat_n('x', n - 1));
                        s
                    }
                    _ => " yes".to_string(),
                };
                let _ = tx.send(Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
                    sequence_id: req.sequence_id,
                    suggestions: vec![
                        omnish_protocol::message::CompletionSuggestion {
                            text: format!("{}{}", trimmed, primary_suffix),
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
            touch_thread(&ctx.active_threads, &cm.thread_id, &ctx.conv_mgr, &ctx.session_mgr).await;
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let req_id = cm.request_id.clone();
            ctx.cancel_flags.lock().await.insert(req_id.clone(), flag.clone());
            handle_chat_message(cm, ctx, &llm, tx, &flag).await;
            ctx.cancel_flags.lock().await.remove(&req_id);
        }
        Message::ChatToolResult(tr) => {
            touch_thread(&ctx.active_threads, &tr.thread_id, &ctx.conv_mgr, &ctx.session_mgr).await;
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
/// already claimed the thread and loaded its raw messages.  Stashes the
/// session's current host/cwd as pending_meta on the claim; `touch_thread`
/// flushes it on the first ChatMessage and, if the host differs, queues the
/// one-shot `<event>host changed ...</event>` marker there (issue #679).
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
    if let Some(claim) = ctx.active_threads.lock().await.get_mut(tid) {
        claim.pending_meta = Some(PendingMeta {
            host: session_meta.host.clone(),
            cwd: session_meta.cwd.clone(),
        });
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
        // If a newer ChatMessage has already bumped this thread's generation,
        // the interrupt is moot - the new turn will sanitize/merge the dangling
        // tool_use itself. Writing here would just race that path.
        if is_superseded(&state, ctx).await {
            tracing::info!(
                "ChatInterrupt arrived for already-superseded loop (thread={}, request={}); skipping",
                ci.thread_id, ci.request_id
            );
            let _ = tx.send(Message::Ack).await;
            return;
        }
        // Build tool_result content: completed results + interrupt marker for the rest.
        // The interrupt marker string matches Claude Code's convention so the LLM
        // sees a well-formed tool_result + later-merged text blocks in a single
        // user message (rather than a synthetic <event> assistant that would
        // break Anthropic's thinking-block/tool-use pairing rules).
        let completed_ids: std::collections::HashSet<String> = state
            .completed_results
            .iter()
            .map(|r| r.tool_use_id.clone())
            .collect();

        let mut result_content: Vec<serde_json::Value> = state
            .completed_results
            .iter()
            .map(|r| tool_result_json(r, call_name(&state.pending_tool_calls, &r.tool_use_id), &ctx.tool_registry))
            .collect();

        // Report how long execution ran: earliest still-running tool, else the
        // LLM call (issue #682).
        let elapsed = state.interrupt_elapsed(
            state
                .pending_tool_calls
                .iter()
                .filter(|tc| !completed_ids.contains(&tc.id))
                .map(|tc| tc.id.as_str()),
        );
        let marker = omnish_daemon::conversation_mgr::interrupt_event_message(Some(elapsed));

        for tc in &state.pending_tool_calls {
            if !completed_ids.contains(&tc.id) {
                result_content.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tc.id,
                    "content": marker,
                    "is_error": true,
                }));
            }
        }
        // Persist the interrupt as a text block too, so the next merge sees the
        // marker (with its elapsed suffix) instead of re-adding a bare one.
        result_content.push(serde_json::json!({
            "type": "text",
            "text": format!("{}\n", marker),
        }));

        persist_unsaved(&mut state, &ctx.conv_mgr, &[
            serde_json::json!({"role": "user", "content": result_content}),
        ]);
        update_thread_usage(&ctx.conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
    } else if let Some(flag) = ctx.cancel_flags.lock().await.get(&ci.request_id) {
        // Agent loop is running daemon-side tools - signal it to stop.
        // The loop will store partial state to conversation when it detects the flag.
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    } else {
        // Loop already finished before the interrupt reached us. Nothing to
        // record - the conversation is already in a consistent state, and
        // adding synthetic messages would only confuse the next LLM call.
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
            // Drop history entries for clients the user pressed Forget on.
            for addr in &effects.client_forgets {
                let removed = ctx.session_mgr.forget_client_addr(addr).await;
                tracing::info!("forget_client: removed {} entries for addr={}", removed, addr);
            }
            // If a backend was added/edited, re-validate so a broken one (e.g.
            // a literal key left in api_key_cmd) surfaces instead of silently
            // vanishing from /model. The hot-reload watcher also rebuilds the
            // backends, but only logs failures; here we report them to clients.
            if effects.backends_changed {
                let (llm, http_proxy, no_proxy) = {
                    let cfg = ctx.opts.daemon_config.read().unwrap();
                    (cfg.llm.clone(), cfg.proxy.http_proxy.clone(), cfg.proxy.no_proxy.clone())
                };
                let failures = omnish_llm::factory::check_backends(
                    &llm,
                    http_proxy.as_deref(),
                    no_proxy.as_deref(),
                );
                if !failures.is_empty() {
                    let text = format!(
                        "backend failed to initialize and won't appear in /model: {}",
                        failures
                            .iter()
                            .map(|(name, err)| format!("'{}' ({})", name, err))
                            .collect::<Vec<_>>()
                            .join("; ")
                    );
                    let senders: Vec<_> = {
                        let map = ctx.push_registry.lock().await;
                        map.values().cloned().collect()
                    };
                    for ptx in senders {
                        let _ = ptx.send(Message::NoticePush {
                            level: NoticeLevel::Error,
                            text: text.clone(),
                            kind: None,
                        }).await;
                    }
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

/// Whether the agent loop should execute this response's tool calls and
/// continue, rather than treating the response as a final turn.
///
/// True when the response carries tool_use blocks, EXCEPT when it was truncated
/// by the output token limit (MaxTokens): such a response may end on an
/// incomplete tool_use (partial input JSON), so we treat it as truncation and
/// fall through to the final path instead of acting on a half-formed call.
///
/// Using block presence rather than `stop_reason == ToolUse` also covers
/// backends that mislabel a tool-calling turn's stop_reason as end_turn
/// (some OpenAI/Anthropic-compatible proxies do this); without this the tool
/// calls would be silently dropped or persisted as orphans.
fn should_execute_tools(response: &omnish_llm::backend::LlmResponse) -> bool {
    !response.tool_calls().is_empty() && response.stop_reason != StopReason::MaxTokens
}

/// Build the assistant message persisted when a turn produced no visible text
/// (empty / thinking-only response). Preserves any thinking blocks so the
/// model's reasoning is recorded rather than lost, and appends `marker` as the
/// text output (keeping the content non-null and sendable). When there is no
/// thinking, stores the bare marker string. tool_use blocks are intentionally
/// excluded: a stored tool_use without a matching tool_result would brick the
/// thread.
fn build_empty_response_message(content: &[ContentBlock], marker: &str) -> serde_json::Value {
    let mut blocks: Vec<serde_json::Value> = content
        .iter()
        .filter(|b| matches!(b, ContentBlock::Thinking { .. }))
        .map(content_block_to_json)
        .collect();
    if blocks.is_empty() {
        serde_json::json!({ "role": "assistant", "content": marker })
    } else {
        blocks.push(serde_json::json!({ "type": "text", "text": marker }));
        serde_json::json!({ "role": "assistant", "content": blocks })
    }
}

/// Chat system prompt: base prompt + system-reminder + optional project
/// instructions. Shared by the chat turn and full `/compact`, whose request
/// must reproduce the turn's prefix byte for byte to hit the prompt cache.
fn compose_chat_system_prompt(base: &str, reminder: &str, project_instructions: Option<&str>) -> String {
    match project_instructions {
        Some(pi) => format!("{}\n\n{}\n\n{}", base, reminder, pi),
        None => format!("{}\n\n{}", base, reminder),
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

    // Bump the thread generation so any agent loop still in-flight on this
    // thread (e.g. an LLM call the user already abandoned by interrupting and
    // sending a new message) becomes stale and discards its work instead of
    // racing us into the conversation file. Captured into AgentLoopState so
    // the new loop knows its own generation.
    let generation = bump_generation(&ctx.thread_generations, &cm.thread_id).await;

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

    // Compact first when the thread is near the context window (issue #691).
    // Must run before the meta below is rewritten for this turn.
    auto_compact_before_turn(&cm, ctx, &effective_backend, &tx).await;

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
    meta.project_instructions = cm.project_instructions.clone();
    // Consume any one-shot event marker queued by an out-of-band change (e.g.
    // /thread sandbox on|off) and clear it in the same save (issue #659).
    let pending_event = meta.pending_event.take();
    conv_mgr.save_meta(&cm.thread_id, &meta);

    // Effective user query: prepend the pending event marker (if any) so the
    // LLM sees the state change inline before the user's text.
    let query: String = match pending_event {
        Some(ev) => format!("{}\n\n{}", ev, cm.query),
        None => cm.query.clone(),
    };

    let full_system_prompt =
        compose_chat_system_prompt(&system_prompt, &reminder, cm.project_instructions.as_deref());

    // Load prior conversation history as raw JSON
    let mut extra_messages = conv_mgr.load_raw_messages(&cm.thread_id);

    // Two independent cleanups:
    //  - sanitize_orphaned_tool_use: handle ChatInterrupt racing ChatMessage
    //    (leaves dangling tool_use without tool_result)
    //  - merge_interrupt_markers: collapse legacy <event>user interrupted</event>
    //    assistant messages from pre-merge builds. Without this, older threads
    //    send a text-only assistant message with no thinking block and trip
    //    DeepSeek/Anthropic validators in thinking mode.
    let mut history_dirty = false;
    if omnish_daemon::conversation_mgr::sanitize_orphaned_tool_use(&mut extra_messages) {
        tracing::warn!("Sanitized orphaned tool_use blocks before chat (thread={})", cm.thread_id);
        history_dirty = true;
    }
    if omnish_daemon::conversation_mgr::merge_interrupt_markers(&mut extra_messages) {
        tracing::warn!("Merged legacy interrupt markers before chat (thread={})", cm.thread_id);
        history_dirty = true;
    }

    // Internal metadata fields (leading underscore, e.g. `_ts`, `_usage`,
    // `_model`) are preserved in storage and stripped at the LLM backend
    // boundary (`omnish_llm::backend::strip_internal_fields`), so individual
    // call sites do not each have to know the current internal field set.

    // Forward-path merge: Anthropic forbids two consecutive user-role
    // messages, so any user-role tail must absorb the new query rather than
    // be followed by a fresh user message. Covers:
    //  - tail = user[tool_result-only]: a fresh interrupt snapshot
    //  - tail = user[tool_result + text + text]: an already-merged tail
    //    (user sent two "继续" in quick succession; the first merge happened,
    //    the second arrives before any assistant response)
    //  - tail = user[text]: a prior user query whose agent loop got
    //    superseded before it could persist any assistant response
    let merge_into_tail = extra_messages.last().is_some_and(
        omnish_daemon::conversation_mgr::is_user_message,
    );
    if merge_into_tail {
        if let Some(last) = extra_messages.last_mut() {
            // The appended query/marker blocks get `now` at the block level while
            // older text already in the tail keeps its own timestamp, so the new
            // turn lands in the current day without pulling the old text with it
            // (issue #680 review).
            omnish_daemon::conversation_mgr::merge_user_query_into_tail(
                last,
                &query,
                ConversationManager::now_ms(),
            );
        }
        conv_mgr.replace_messages(&cm.thread_id, &extra_messages);
        history_dirty = false; // replace_messages covered both cleanups + merge
    } else {
        // Tail is assistant (or empty) - append a fresh user message. Stamp it
        // explicitly because replace_messages (history_dirty path) won't.
        let mut user_msg = serde_json::json!({"role": "user", "content": query});
        ConversationManager::stamp_message_timestamp(
            &mut user_msg,
            ConversationManager::now_ms(),
        );
        extra_messages.push(user_msg.clone());
        if history_dirty {
            conv_mgr.replace_messages(&cm.thread_id, &extra_messages);
            history_dirty = false;
        } else {
            // Persist user message immediately so /resume works even if the
            // agent loop hasn't finished (each message is handled in its own
            // spawned task, so ChatEnd can race with the agent loop).
            conv_mgr.append_messages(&cm.thread_id, &[user_msg]);
        }
    }
    let _ = history_dirty; // future-proofing if new cleanup branches are added

    // Everything in extra_messages has been persisted by this point (either
    // append_messages for the fresh user turn, or replace_messages for the
    // merge / cleanup paths).
    let saved_up_to = extra_messages.len();

    // Non-destructive /compact mini: truncate old bulky tool results in the
    // request copy only. Everything above is already persisted and the agent
    // loop only appends messages past `saved_up_to`, so this view never
    // reaches storage.
    conv_mgr.apply_mini_compact_view(&cm.thread_id, &mut extra_messages);

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
        saved_up_to,
        pending_tool_calls: vec![],
        completed_results: vec![],
        iteration: 0,
        cm,
        start: std::time::Instant::now(),
        last_active: std::time::Instant::now(),
        command_query_tool,
        effective_backend,
        llm_retries: 0,
        cumulative_usage: Default::default(),
        last_response_usage: Default::default(),
        last_model: String::new(),
        generation,
        llm_call_started: None,
        tool_started: HashMap::new(),
        auto_compact_failed: false,
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

    // If a newer ChatMessage has bumped the thread generation, this tool
    // result belongs to a turn the user already abandoned. Drop the state
    // and acknowledge without resuming - the new turn owns the conversation.
    if state.generation != current_generation(&ctx.thread_generations, &state.cm.thread_id).await {
        tracing::info!(
            "ChatToolResult for superseded loop (thread={}, request={}); discarding",
            state.cm.thread_id, tr.request_id
        );
        map.remove(&tr.request_id);
        let _ = tx.send(Message::Ack).await;
        return;
    }

    // Progress: a tool result arrived, so this loop is not abandoned. Refresh the
    // idle clock the stale-loop sweep uses (covers slow batches where results
    // trickle in one at a time).
    state.last_active = std::time::Instant::now();

    // Add the received client-side tool result
    let tool_call_id = tr.tool_call_id.clone();
    let mut content = tr.content;

    let tool_name = call_name(&state.pending_tool_calls, &tool_call_id).map(String::from);
    let image_desc = if tr.is_error {
        None
    } else {
        image_description(&content, tool_name.as_deref(), tool_registry)
    };

    // LLM summarization for client-side tool results (never for images)
    if tr.needs_summarization && !tr.is_error && image_desc.is_none() {
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
            // An image result shows its description, never the base64.
            let output = image_desc.clone().unwrap_or_else(|| result.content.clone());
            let fmt_out = formatter_mgr.format(&formatter_name, &omnish_plugin::formatter::FormatInput {
                tool_name: tc.name.clone(),
                params: tc.input.clone(),
                output,
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
        .map(|r| tool_result_json(r, call_name(&state.pending_tool_calls, &r.tool_use_id), tool_registry))
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
async fn persist_unsaved_sanitized(
    state: &mut AgentLoopState,
    ctx: &HandlerCtx,
) {
    // If a newer ChatMessage has bumped the thread generation, this loop's
    // partial state is no longer authoritative; skip the write so we don't
    // race the new turn into the conversation file.
    if is_superseded(state, ctx).await {
        return;
    }
    let conv_mgr = &ctx.conv_mgr;
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

        // These tools never got dispatched, so the elapsed falls back to the LLM
        // call that produced them (issue #682).
        let elapsed = state.interrupt_elapsed(tool_ids.iter().map(|id| id.as_str()));
        let marker = omnish_daemon::conversation_mgr::interrupt_event_message(Some(elapsed));

        let mut result_content: Vec<serde_json::Value> = tool_ids.iter().map(|id| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": marker,
                "is_error": true,
            })
        }).collect();
        result_content.push(serde_json::json!({
            "type": "text",
            "text": format!("{}\n", marker),
        }));

        state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
            serde_json::json!({
                "role": "user",
                "content": result_content,
            }),
            omnish_llm::backend::CacheHint::None,
        ));
    }

    // No synthetic assistant marker: the next chat message will merge its
    // text into the trailing user[tool_result] message, which keeps the
    // conversation Anthropic-protocol-valid.
    persist_unsaved(state, conv_mgr, &[]);
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

/// True when the loop's saved generation no longer matches the thread's
/// current generation - meaning a newer ChatMessage has arrived and any
/// progress this loop is about to commit would corrupt the conversation.
async fn is_superseded(state: &AgentLoopState, ctx: &HandlerCtx) -> bool {
    let current = current_generation(&ctx.thread_generations, &state.cm.thread_id).await;
    if current != state.generation {
        tracing::info!(
            "Agent loop superseded (thread={}, loop_gen={}, current_gen={}); discarding work",
            state.cm.thread_id, state.generation, current
        );
        true
    } else {
        false
    }
}

/// Auto compaction between the LLM calls of an agent turn (issue #691).
/// Checked only once this loop has a response: its reported usage covers the
/// request up to that response, and the tool results appended since are
/// estimated. On compaction the unsaved messages are persisted first, the
/// summary replays this loop's own system prompt and tools, and the request
/// restarts from the new window (the summary, which asks the model to resume
/// the task). Returns true when the thread was compacted.
async fn auto_compact_mid_turn(
    state: &mut AgentLoopState,
    ctx: &HandlerCtx,
    tx: &mpsc::Sender<Message>,
) -> bool {
    if state.auto_compact_failed {
        return false;
    }
    let u = &state.last_response_usage;
    let Some(last_usage) = usage_tokens(u.input_tokens, u.cache_read_input_tokens, u.cache_creation_input_tokens, u.output_tokens) else {
        return false;
    };
    let percent = auto_compact_percent(ctx);
    let Some(context_window) = state.effective_backend.context_window().filter(|_| percent > 0) else {
        return false;
    };
    let refs: Vec<&serde_json::Value> = state.llm_req.extra_messages.iter().map(|m| &m.content).collect();
    let used = omnish_daemon::compact::estimate_context_tokens(Some(last_usage), &refs);
    let messages = omnish_daemon::compact::estimate_context_tokens(None, &refs);
    if !omnish_daemon::compact::auto_compact_due(used, messages, Some(context_window), percent) {
        return false;
    }
    // Never write for a loop a newer message has superseded.
    if is_superseded(state, ctx).await {
        return false;
    }
    persist_unsaved(state, &ctx.conv_mgr, &[]);
    let replay = CompactReplay {
        system_prompt: state.llm_req.system_prompt.as_ref().map(|p| p.text.clone()).unwrap_or_default(),
        tools: state.llm_req.tools.clone(),
    };
    let backend = state.effective_backend.clone();
    let tid = state.cm.thread_id.clone();
    if !run_auto_compact(&state.cm, ctx, &backend, used, context_window, CompactTrigger::AutoMidTurn, Some(replay), tx).await {
        state.auto_compact_failed = true;
        return false;
    }
    let mut window = ctx.conv_mgr.load_raw_messages(&tid);
    ctx.conv_mgr.apply_mini_compact_view(&tid, &mut window);
    state.saved_up_to = window.len();
    state.llm_req.extra_messages = window
        .into_iter()
        .map(|content| omnish_llm::backend::TaggedMessage::new(content, omnish_llm::backend::CacheHint::None))
        .collect();
    // The reported usage measured the replaced context.
    state.last_response_usage = Default::default();
    true
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
    let backend = state.effective_backend.clone();

    let max_iterations = 100;

    for iteration in state.iteration..max_iterations {
        // Supersession trumps cancellation: if a newer ChatMessage has bumped
        // the thread generation, drop this loop entirely without writing.
        if is_superseded(&state, ctx).await {
            return;
        }

        // Check if user interrupted (Ctrl+C)
        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("Agent loop cancelled by user at iteration {} (thread={})", iteration, state.cm.thread_id);
            persist_unsaved_sanitized(&mut state, ctx).await;
            return;
        }

        // Compact when tool results pushed the context near the window.
        if auto_compact_mid_turn(&mut state, ctx, &tx).await && is_superseded(&state, ctx).await {
            return;
        }

        // Apply cache hints fresh each iteration: agent loop appends new messages,
        // and last-N markers must roll forward without accumulating beyond budget.
        mark_chat_message_hints(&mut state.llm_req.extra_messages);

        // Record when this LLM call started so an interrupt while waiting on it
        // can report the elapsed time (issue #682).
        state.llm_call_started = Some(std::time::Instant::now());
        match omnish_daemon::llm_pool::complete(backend.as_ref(), &state.llm_req).await {
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

                if should_execute_tools(&response) {
                    let tool_calls = response.tool_calls();

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
                                    persist_unsaved_sanitized(&mut state, ctx).await;
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
                        // Record the tool's start for interrupt elapsed reporting
                        // (issue #682).
                        state.tool_started.insert(tc.id.clone(), std::time::Instant::now());
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
                            persist_unsaved_sanitized(&mut state, ctx).await;
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
                                persist_unsaved_sanitized(&mut state, ctx).await;
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
                            // An image result shows its description, never the base64.
                            let post_output = if result.is_error {
                                None
                            } else {
                                image_description(&result.content, Some(&tc.name), tool_registry)
                            }
                            .unwrap_or_else(|| result.content.clone());
                            let post_out = formatter_mgr.format(&post_fmt_name, &omnish_plugin::formatter::FormatInput {
                                tool_name: tc.name.clone(),
                                params: tc.input.clone(),
                                output: post_output,
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
                                persist_unsaved_sanitized(&mut state, ctx).await;
                                return;
                            }

                            tool_results.push(result);
                        }
                    }

                    if cancelled {
                        // Cancelled mid-tool-execution - store partial state.
                        // Skip if a newer ChatMessage has superseded this loop:
                        // writing partial tool_results would duplicate the
                        // synthetic ones the new turn already produced.
                        if is_superseded(&state, ctx).await {
                            return;
                        }
                        tracing::info!("Agent loop cancelled during tool execution (thread={})", state.cm.thread_id);
                        let completed_ids: std::collections::HashSet<&str> = tool_results
                            .iter()
                            .map(|r| r.tool_use_id.as_str())
                            .collect();
                        let mut result_content: Vec<serde_json::Value> = tool_results
                            .iter()
                            .map(|r| tool_result_json(r, call_name(tool_calls.iter().copied(), &r.tool_use_id), tool_registry))
                            .collect();
                        let elapsed = state.interrupt_elapsed(
                            tool_calls
                                .iter()
                                .filter(|tc| !completed_ids.contains(tc.id.as_str()))
                                .map(|tc| tc.id.as_str()),
                        );
                        let marker = omnish_daemon::conversation_mgr::interrupt_event_message(Some(elapsed));
                        for tc in &tool_calls {
                            if !completed_ids.contains(tc.id.as_str()) {
                                result_content.push(serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": tc.id,
                                    "content": marker,
                                    "is_error": true,
                                }));
                            }
                        }
                        result_content.push(serde_json::json!({
                            "type": "text",
                            "text": format!("{}\n", marker),
                        }));
                        state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                            serde_json::json!({
                                "role": "user",
                                "content": result_content,
                            }),
                            omnish_llm::backend::CacheHint::None,
                        ));
                        persist_unsaved(&mut state, conv_mgr, &[]);
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
                        // Dispatching work: reset the idle clock so the sweep only
                        // reclaims this loop if the client goes silent past the
                        // (tool-timeout-aware) deadline.
                        state.last_active = std::time::Instant::now();
                        let request_id = state.cm.request_id.clone();
                        pending_loops.lock().await.insert(request_id, state);
                        return; // tx dropped → Ack sent by spawn_connection
                    }

                    // All tools were daemon-side - build tool_result and continue
                    let result_content: Vec<serde_json::Value> = tool_results
                        .iter()
                        .map(|r| tool_result_json(r, call_name(tool_calls.iter().copied(), &r.tool_use_id), tool_registry))
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

                // EndTurn or MaxTokens - extract final text and store.
                // If a newer ChatMessage has superseded us, drop the response
                // silently: the next turn won't see this assistant text and
                // the user already moved on.
                if is_superseded(&state, ctx).await {
                    return;
                }
                let text = response.text();
                tracing::info!(
                    "Chat LLM completed in {:?} ({} tool iterations, thread={})",
                    state.start.elapsed(),
                    iteration,
                    state.cm.thread_id
                );

                // Guard against an empty visible answer. This happens when the
                // model returns thinking but no text - most commonly when the
                // API truncates the response during the thinking phase
                // (stop_reason == MaxTokens, finish_reason "length"), leaving
                // only a Thinking block. Persisting such a turn would store an
                // assistant message whose only content is the thinking block;
                // for OpenAI-compat backends (DeepSeek) that serializes to
                // `content: null`, which the API rejects, permanently bricking
                // the thread. Degrade to a sendable `<event>` marker instead.
                if text.trim().is_empty() {
                    let (marker, notice) = if response.stop_reason == StopReason::MaxTokens {
                        (
                            "<event>response truncated (max tokens)</event>",
                            "The response was truncated (output token limit reached). Send another message to continue.",
                        )
                    } else {
                        (
                            "<event>empty response</event>",
                            "The AI returned an empty response. Send another message to continue.",
                        )
                    };
                    let has_thinking = response.content.iter().any(|b| matches!(b, ContentBlock::Thinking { .. }));
                    tracing::warn!(
                        "Chat LLM returned empty text (stop_reason={:?}, has_thinking={}, thread={}); persisting placeholder",
                        response.stop_reason,
                        has_thinking,
                        state.cm.thread_id
                    );
                    // Preserve the model's thinking (see build_empty_response_message)
                    // so the reasoning is recorded rather than overwritten by the marker.
                    let assistant_msg = build_empty_response_message(&response.content, marker);
                    state.llm_req.extra_messages.push(omnish_llm::backend::TaggedMessage::new(
                        assistant_msg,
                        omnish_llm::backend::CacheHint::None,
                    ));
                    persist_unsaved(&mut state, conv_mgr, &[]);
                    update_thread_usage(conv_mgr, &state.cm.thread_id, &state.last_response_usage, &state.cumulative_usage, &state.last_model);
                    let _ = tx.send(Message::ChatResponse(ChatResponse {
                        request_id: state.cm.request_id.clone(),
                        thread_id: state.cm.thread_id.clone(),
                        content: notice.to_string(),
                    })).await;
                    return;
                }

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
                let err_str = format!("{:#}", e);
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
                    // Surface the retry so the client shows progress instead of a
                    // silent spinner during the backoff sleep (issue #658).
                    let _ = tx.send(Message::ChatToolStatus(ChatToolStatus {
                        request_id: state.cm.request_id.clone(),
                        thread_id: state.cm.thread_id.clone(),
                        tool_name: String::new(),
                        status: format!(
                            "LLM connection error, retrying ({}/2) in {}s...",
                            state.llm_retries, backoff.as_secs()
                        ),
                        tool_call_id: None,
                        status_icon: None,
                        display_name: None,
                        param_desc: None,
                        result_compact: None,
                        result_full: None,
                    })).await;
                    tokio::time::sleep(backoff).await;
                    continue;
                }

                tracing::error!("Chat LLM failed: {:#}", e);

                // If a newer ChatMessage has superseded us, swallow the error
                // entirely: the user has already moved on and the new turn
                // owns the conversation tail.
                if is_superseded(&state, ctx).await {
                    return;
                }

                // User-visible message: surface the precise backend error. The
                // backend error string already carries HTTP status + error type +
                // API message, so keep enough of it to be actionable rather than
                // collapsing it into a vague "interrupted" (issue #658). The
                // persisted history marker below stays short to avoid context bloat.
                let display_err: &str = if err_str.len() > 500 {
                    &err_str[..err_str.floor_char_boundary(500)]
                } else {
                    &err_str
                };
                let user_msg = if is_connection {
                    format!("[!] LLM connection error: {}\n\nYour progress has been saved - send another message to continue.", display_err)
                } else {
                    format!("[!] LLM error: {}\n\nYour progress has been saved - send another message to continue.", display_err)
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

    // Exhausted iterations - store what we have. Skip if superseded.
    if is_superseded(&state, ctx).await {
        return;
    }
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

    match omnish_daemon::llm_pool::complete(backend.as_ref(), &req).await {
        Ok(_) => tracing::debug!("KV cache warmup completed for session {}", session_id),
        Err(e) => tracing::debug!("KV cache warmup failed for session {}: {}", session_id, e),
    }
}

/// Build the user message's content blocks for a completion-style request.
///
/// Layout: `[stable_prefix, remainder, cwd_history, query]` with `cache_control`
/// on the stable_prefix block (cache_pos=0). Between warmups, Block 0 is
/// byte-stable so Anthropic's KV cache hits on the cached breakpoint. The
/// cwd_history block depends on per-request input and is never cached; it sits
/// after remainder so it's the last context the LLM sees before the query.
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
    if !sections.cwd_history.is_empty() {
        blocks.push(serde_json::json!({
            "type": "text",
            "text": sections.cwd_history,
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
/// Parse a tool result as an image (issue #690), only for tools that declare
/// `result_images` in their tool.json and only for successful results, so no
/// other tool's text can ever be taken for an image.
fn parse_tool_image(
    content: &str,
    tool_name: Option<&str>,
    registry: &omnish_daemon::tool_registry::ToolRegistry,
) -> Option<omnish_common::tool_image::ToolImage> {
    if !tool_name.is_some_and(|n| registry.result_images(n)) {
        return None;
    }
    let img = omnish_common::tool_image::parse(content);
    if img.is_none() {
        tracing::warn!(
            "tool {} declares result_images but returned no valid image; storing as text",
            tool_name.unwrap_or("?")
        );
    }
    img
}

/// The description text of an image tool result, used instead of the raw
/// content for display and summarization decisions.
fn image_description(
    content: &str,
    tool_name: Option<&str>,
    registry: &omnish_daemon::tool_registry::ToolRegistry,
) -> Option<String> {
    parse_tool_image(content, tool_name, registry).map(|img| img.description)
}

/// Build the stored `tool_result` block for a tool result. An image result
/// gets Anthropic-format content `[description text, image]`; everything else
/// keeps its plain string content.
fn tool_result_json(
    r: &omnish_llm::tool::ToolResult,
    tool_name: Option<&str>,
    registry: &omnish_daemon::tool_registry::ToolRegistry,
) -> serde_json::Value {
    let image = if r.is_error { None } else { parse_tool_image(&r.content, tool_name, registry) };
    let content = match image {
        Some(img) => serde_json::json!([
            {"type": "text", "text": img.description},
            {"type": "image", "source": {
                "type": "base64",
                "media_type": img.media_type,
                "data": img.data_base64,
            }},
        ]),
        None => serde_json::Value::String(r.content.clone()),
    };
    serde_json::json!({
        "type": "tool_result",
        "tool_use_id": r.tool_use_id,
        "content": content,
        "is_error": r.is_error,
    })
}

/// Text of a stored tool_result's content: the string itself, or the joined
/// text blocks of an array (an image result shows its description).
fn tool_result_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Name of the tool call with `id`.
fn call_name<'a>(
    calls: impl IntoIterator<Item = &'a omnish_llm::tool::ToolCall>,
    id: &str,
) -> Option<&'a str> {
    calls.into_iter().find(|tc| tc.id == id).map(|tc| tc.name.as_str())
}

/// Transcript cap for a full compaction when the backend sets no
/// `max_content_chars`. The live window already fits the chat model (it is
/// sent in full every turn) and the transcript clips tool I/O, so this only
/// guards pathological threads.
const COMPACT_TRANSCRIPT_MAX_CHARS: usize = 300_000;

/// Percentage by which `after` shrank relative to `before`.
fn shrink_pct(before: usize, after: usize) -> f64 {
    if before > 0 {
        (1.0 - after as f64 / before as f64) * 100.0
    } else {
        0.0
    }
}

/// Result of a full compaction, for the command output.
struct FullCompactOutcome {
    /// Live context size before/after, in serialized chars.
    chars_before: usize,
    chars_after: usize,
    /// Usage of the summarization call (cache hit tokens show whether the
    /// replayed prefix was served from the provider cache).
    usage: Option<omnish_llm::backend::Usage>,
    /// Set when the cache-friendly replay failed and the transcript path was
    /// used instead; holds the reason.
    fallback: Option<String>,
}

/// Build the cache-friendly compaction request: the chat turn replayed
/// verbatim (same system prompt, tools, thinking mode, history and cache
/// breakpoint scheme) with the compaction instruction as the final user
/// message. Only that last message differs from the previous chat request,
/// so providers with prefix caching (Anthropic, DeepSeek) serve the rest
/// from cache. This mirrors how Claude Code produces its /compact summary.
fn build_compact_turn_request(
    window: &[serde_json::Value],
    system_prompt: String,
    tools: Vec<omnish_llm::tool::ToolDef>,
    instruction: &str,
    session_id: &str,
    max_content_chars: Option<usize>,
) -> LlmRequest {
    // Same cleanups the chat path applies before sending (on a copy; the
    // stored history is left alone).
    let mut msgs = window.to_vec();
    omnish_daemon::conversation_mgr::sanitize_orphaned_tool_use(&mut msgs);
    omnish_daemon::conversation_mgr::merge_interrupt_markers(&mut msgs);
    omnish_daemon::compact::append_compact_instruction(&mut msgs, instruction, ConversationManager::now_ms());
    let mut extra_messages: Vec<omnish_llm::backend::TaggedMessage> = msgs
        .into_iter()
        .map(|content| omnish_llm::backend::TaggedMessage::new(content, omnish_llm::backend::CacheHint::None))
        .collect();
    mark_chat_message_hints(&mut extra_messages);
    LlmRequest {
        context: String::new(),
        query: None,
        trigger: TriggerType::Manual,
        session_ids: vec![session_id.to_string()],
        use_case: UseCase::Chat,
        max_content_chars,
        system_prompt: Some(omnish_llm::backend::CachedText {
            text: system_prompt,
            cache: omnish_llm::backend::CacheHint::Long,
        }),
        // Must match the chat turn: toggling thinking invalidates the cached
        // message prefix on Anthropic.
        enable_thinking: Some(true),
        tools,
        extra_messages,
    }
}

/// Build the fallback compaction request: the live window rendered as a
/// plain-text transcript. Shares no prefix with the chat request (no cache
/// reuse) but is smaller, since tool I/O is clipped.
fn build_compact_transcript_request(
    window: &[serde_json::Value],
    budget: usize,
    language: &str,
    max_content_chars: Option<usize>,
) -> LlmRequest {
    let transcript = omnish_daemon::compact::build_transcript(window, budget);
    LlmRequest {
        context: format!("<conversation>\n{}\n</conversation>", transcript),
        query: Some(omnish_llm::template::append_language_instruction(
            omnish_llm::template::COMPACT_PROMPT,
            language,
        )),
        trigger: TriggerType::Manual,
        session_ids: vec![],
        use_case: UseCase::Chat,
        max_content_chars,
        system_prompt: None,
        enable_thinking: Some(false),
        tools: vec![],
        extra_messages: vec![],
    }
}

/// Extract the summary text from a compaction response. A tool call means the
/// model ignored the instruction (tools are offered only to keep the prefix
/// identical), so it counts as a failure rather than a partial summary.
fn compact_summary_from_response(resp: &omnish_llm::backend::LlmResponse) -> Result<String, String> {
    if !resp.tool_calls().is_empty() {
        return Err("model called a tool instead of summarizing".to_string());
    }
    let summary = omnish_daemon::strip_thinking_block(&resp.text()).trim().to_string();
    if summary.is_empty() {
        return Err("LLM returned an empty summary".to_string());
    }
    Ok(summary)
}

/// What triggered a full compaction.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CompactTrigger {
    /// The `/compact` command.
    Command,
    /// Auto compaction before a new user message is added (issue #691).
    AutoBeforeTurn,
    /// Auto compaction between LLM calls of an agent turn (issue #691): the
    /// summary must tell the model to resume the task on its own.
    AutoMidTurn,
}

/// System prompt and tools a compaction replays, for the cache-friendly
/// request. The agent loop passes what its own request carries; otherwise
/// they are rebuilt from the thread meta.
struct CompactReplay {
    system_prompt: String,
    tools: Vec<omnish_llm::tool::ToolDef>,
}

/// Backend a thread chats with: its per-thread model override, else the chat
/// use case's backend.
fn thread_backend(llm: &MultiBackend, conv_mgr: &ConversationManager, tid: &str) -> Arc<dyn LlmBackend> {
    conv_mgr.load_meta(tid).model.as_ref()
        .and_then(|name| llm.get_backend_by_name(name))
        .unwrap_or_else(|| llm.get_backend(UseCase::Chat))
}

/// Full context compaction (issue #687): summarize the live window with the
/// thread's chat model and append the summary as a `_compact` user message.
/// Nothing is deleted; `load_raw_messages` starts from the new message.
///
/// First tries the cache-friendly replay of the chat request
/// (`build_compact_turn_request`); on any failure (e.g. the thread is too
/// close to the context window, or the model calls a tool) falls back to the
/// transcript request. Returns None when there is nothing to compact.
async fn full_compact_thread(
    tid: &str,
    session_id: &str,
    ctx: &HandlerCtx,
    backend: &Arc<dyn LlmBackend>,
    trigger: CompactTrigger,
    replay: Option<CompactReplay>,
) -> Result<Option<FullCompactOutcome>, String> {
    use omnish_daemon::compact;
    let conv_mgr = &ctx.conv_mgr;
    let (mut window, total) = conv_mgr
        .snapshot_for_compact(tid)
        .ok_or_else(|| "thread not found".to_string())?;
    if !compact::has_content_to_compact(&window) {
        return Ok(None);
    }
    // Same view the chat request sends (recorded /compact mini applied), so
    // the replay matches the cached prefix.
    conv_mgr.apply_mini_compact_view(tid, &mut window);

    let mut meta = conv_mgr.load_meta(tid);
    let max_content_chars = backend.max_content_chars();
    let language = ctx.opts.daemon_config.read().unwrap().client.language.clone();

    // Primary: replay the last chat turn's prefix. The reminder and project
    // instructions come from the thread meta (what that turn actually sent),
    // not rebuilt, so e.g. a date change does not break the prefix. Threads
    // last chatted before this was recorded rebuild the reminder instead.
    let CompactReplay { system_prompt, tools } = match replay {
        Some(r) => r,
        None => {
            let ChatSetup { command_query_tool, tools, system_prompt } =
                build_chat_setup(&ctx.session_mgr, &ctx.tool_registry).await;
            let reminder = match meta.system_reminder.clone() {
                Some(r) => r,
                None => {
                    let attrs = ctx.session_mgr.get_session_attrs(session_id).await;
                    command_query_tool.build_system_reminder(session_id, 5, &attrs, false)
                }
            };
            CompactReplay {
                system_prompt: compose_chat_system_prompt(&system_prompt, &reminder, meta.project_instructions.as_deref()),
                tools,
            }
        }
    };
    let turn_req = build_compact_turn_request(
        &window,
        system_prompt,
        tools,
        &omnish_llm::template::append_language_instruction(
            omnish_llm::template::COMPACT_TURN_PROMPT,
            &language,
        ),
        session_id,
        max_content_chars,
    );
    tracing::info!("Full compaction ({:?}) of thread {}: replaying {} messages", trigger, tid, window.len());
    let primary = match omnish_daemon::llm_pool::complete(backend.as_ref(), &turn_req).await {
        Ok(resp) => compact_summary_from_response(&resp).map(|s| (s, resp.usage)),
        Err(e) => Err(format!("LLM request failed: {}", e)),
    };

    let (summary, usage, fallback) = match primary {
        Ok((summary, usage)) => (summary, usage, None),
        Err(reason) => {
            tracing::warn!(
                "Full compaction of thread {}: replay failed ({}), falling back to transcript",
                tid, reason
            );
            let budget = max_content_chars.unwrap_or(COMPACT_TRANSCRIPT_MAX_CHARS);
            let req = build_compact_transcript_request(&window, budget, &language, max_content_chars);
            let resp = omnish_daemon::llm_pool::complete(backend.as_ref(), &req)
                .await
                .map_err(|e| format!("LLM request failed: {}", e))?;
            (compact_summary_from_response(&resp)?, resp.usage, Some(reason))
        }
    };

    let mut text = format!("{}\n\n{}", omnish_llm::template::COMPACT_SUMMARY_HEADER, summary);
    if trigger == CompactTrigger::AutoMidTurn {
        text.push_str("\n\n");
        text.push_str(omnish_llm::template::COMPACT_CONTINUE_HINT);
    }
    let mut msg = compact::make_compact_message(&text, ConversationManager::now_ms());
    if trigger != CompactTrigger::Command {
        msg[compact::COMPACT_AUTO_FIELD] = serde_json::json!(true);
    }
    let chars_before = compact::messages_chars(&window);
    let chars_after = compact::messages_chars(std::slice::from_ref(&msg));
    conv_mgr.append_compact_summary(tid, total, msg)?;
    // The last call's usage measured the context that was just replaced; drop
    // it so auto compaction estimates the new window instead of re-firing.
    meta = conv_mgr.load_meta(tid);
    meta.usage_last = None;
    conv_mgr.save_meta(tid, &meta);
    tracing::info!(
        "Compacted thread {}: context {} -> {} chars, usage {:?}, fallback {:?}",
        tid, chars_before, chars_after, usage, fallback
    );
    Ok(Some(FullCompactOutcome { chars_before, chars_after, usage, fallback }))
}

/// Total tokens of an LLM call (input, cached and output), the size of the
/// context it leaves behind. None when the provider reported nothing.
fn usage_tokens(input: u64, cache_read: u64, cache_creation: u64, output: u64) -> Option<u64> {
    Some(input + cache_read + cache_creation + output).filter(|&n| n > 0)
}

/// Auto compaction threshold from the daemon config (issue #691).
fn auto_compact_percent(ctx: &HandlerCtx) -> u32 {
    ctx.opts.daemon_config.read().unwrap().llm.auto_compact_percent
}

/// Stream a one-line progress note into the chat turn (rendered like LLM
/// intermediate text).
async fn send_chat_note(tx: &mpsc::Sender<Message>, cm: &ChatMessage, text: String) {
    let _ = tx.send(Message::ChatToolStatus(ChatToolStatus {
        request_id: cm.request_id.clone(),
        thread_id: cm.thread_id.clone(),
        tool_name: String::new(),
        status: text,
        tool_call_id: None,
        status_icon: None,
        display_name: None,
        param_desc: None,
        result_compact: None,
        result_full: None,
    })).await;
}

/// Run an auto compaction (issue #691) whose threshold check already passed,
/// reporting progress on `tx`. Failures are logged and reported; the caller
/// carries on with the uncompacted context. Returns true when compacted.
#[allow(clippy::too_many_arguments)]
async fn run_auto_compact(
    cm: &ChatMessage,
    ctx: &HandlerCtx,
    backend: &Arc<dyn LlmBackend>,
    used: u64,
    context_window: usize,
    trigger: CompactTrigger,
    replay: Option<CompactReplay>,
    tx: &mpsc::Sender<Message>,
) -> bool {
    let pct = used * 100 / context_window.max(1) as u64;
    tracing::info!(
        "Auto compaction ({:?}) of thread {}: ~{} of {} tokens ({}%)",
        trigger, cm.thread_id, used, context_window, pct
    );
    send_chat_note(tx, cm, format!(
        "Context at {}% of {} tokens, compacting...",
        pct, format_count(context_window as u64)
    )).await;
    match full_compact_thread(&cm.thread_id, &cm.session_id, ctx, backend, trigger, replay).await {
        Ok(Some(o)) => {
            send_chat_note(tx, cm, format!(
                "Context compacted: {} -> {} chars (-{:.0}%)",
                format_count(o.chars_before as u64),
                format_count(o.chars_after as u64),
                shrink_pct(o.chars_before, o.chars_after)
            )).await;
            true
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!("Auto compaction of thread {} failed: {}", cm.thread_id, e);
            send_chat_note(tx, cm, format!("Auto compaction failed: {}", e)).await;
            false
        }
    }
}

/// Auto compaction before a user message is added to the thread (issue
/// #691). The estimate is the last call's reported usage plus the messages
/// after it and the new query. Runs before the chat turn rewrites the thread
/// meta, so the replay uses the system prompt of the turn that filled the
/// cache. The query is then merged into the summary message as usual.
async fn auto_compact_before_turn(
    cm: &ChatMessage,
    ctx: &HandlerCtx,
    backend: &Arc<dyn LlmBackend>,
    tx: &mpsc::Sender<Message>,
) {
    let percent = auto_compact_percent(ctx);
    let Some(context_window) = backend.context_window().filter(|_| percent > 0) else {
        return;
    };
    // A loop parked on client tools owns the thread tail; leave it alone (the
    // /compact command refuses in the same case).
    if ctx.pending_loops.lock().await.values().any(|s| s.cm.thread_id == cm.thread_id) {
        return;
    }
    let conv_mgr = &ctx.conv_mgr;
    let window = conv_mgr.load_raw_messages(&cm.thread_id);
    let last_usage = conv_mgr.load_meta(&cm.thread_id).usage_last.and_then(|u| {
        usage_tokens(u.input_tokens, u.cache_read_input_tokens, u.cache_creation_input_tokens, u.output_tokens)
    });
    let refs: Vec<&serde_json::Value> = window.iter().collect();
    let query_tokens = cm.query.len().div_ceil(4) as u64;
    let used = omnish_daemon::compact::estimate_context_tokens(last_usage, &refs) + query_tokens;
    let messages = omnish_daemon::compact::estimate_context_tokens(None, &refs);
    if !omnish_daemon::compact::auto_compact_due(used, messages, Some(context_window), percent) {
        return;
    }
    run_auto_compact(cm, ctx, backend, used, context_window, CompactTrigger::AutoBeforeTurn, None, tx).await;
}

/// Command output for a full compaction.
fn format_full_compact(o: &FullCompactOutcome) -> String {
    let mut out = format!(
        "context compacted into a summary\ncontext: {} -> {} chars (-{:.0}%)",
        format_count(o.chars_before as u64),
        format_count(o.chars_after as u64),
        shrink_pct(o.chars_before, o.chars_after)
    );
    if let Some(u) = &o.usage {
        let input = u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
        out.push_str(&format!(
            "\nLLM input: {} tokens ({} cached)",
            format_count(input),
            format_count(u.cache_read_input_tokens)
        ));
    }
    if let Some(reason) = &o.fallback {
        out.push_str(&format!("\n(summarized from transcript: {})", reason));
    }
    out
}

fn cmd_display(s: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "display": s.into() })
}

/// Like `cmd_display` but attaches a machine-readable `status` so the client
/// can render a localized message instead of the English `display` fallback.
fn cmd_status(display: impl Into<String>, status: &str) -> serde_json::Value {
    serde_json::json!({ "display": display.into(), "status": status })
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
                    // Mixed array: may contain tool_result blocks (interrupt
                    // snapshot) plus text blocks for the merged user query.
                    for (idx, block) in arr.iter().enumerate() {
                        // Full-compaction summary (issue #687): show it as the
                        // /compact command and its result. A `response` entry
                        // (not `llm_text`) so the client renders the summary's
                        // markdown like any final assistant reply.
                        if omnish_daemon::compact::is_compact_summary_block(msg, idx) {
                            let label = if omnish_daemon::compact::is_auto_compact_message(msg) {
                                "/compact (auto)"
                            } else {
                                "/compact"
                            };
                            entries.push(serde_json::json!({
                                "type": "user_input",
                                "text": label,
                            }));
                            entries.push(serde_json::json!({
                                "type": "response",
                                "text": block["text"].as_str().unwrap_or(""),
                            }));
                            entries.push(serde_json::json!({"type": "separator"}));
                            continue;
                        }
                        match block["type"].as_str() {
                            Some("tool_result") => {
                                let tool_use_id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                                let output = tool_result_text(&block["content"]);
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
                            Some("text") => {
                                // Skip the interrupt marker (with or without the
                                // elapsed suffix); render the rest as user input.
                                let text = block["text"].as_str().unwrap_or("");
                                if omnish_daemon::conversation_mgr::is_interrupt_marker(text)
                                    || text.is_empty()
                                {
                                    continue;
                                }
                                entries.push(serde_json::json!({
                                    "type": "user_input",
                                    "text": text,
                                }));
                            }
                            _ => {}
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
                // extract_text hides the summary block; show it here.
                if omnish_daemon::compact::is_compact_message(msg) {
                    if let Some(summary) = msg["content"][0]["text"].as_str() {
                        output.push_str(&format!("[Compact summary] {}\n\n", summary));
                    }
                }
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
        // Mirror the scheduled job's cron-derived window so the preview matches
        // what the next run will summarize.
        let hourly_since = {
            let cfg = ctx.opts.daemon_config.read().unwrap();
            let schedule = cfg
                .tasks
                .get("hourly_summary")
                .map(|c| omnish_daemon::task_mgr::normalize_cron(&c.get_string("schedule", "")))
                .unwrap_or_default();
            omnish_daemon::hourly_summary::preview_window_start(&schedule, chrono::Local::now())
        };
        return cmd_display(
            handle_context_scenario(scenario, req, mgr, llm_backend, conv_mgr, hourly_since).await,
        );
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
    // Handle /conversations del <thread_id> - soft-delete by thread ID (#684).
    // The conversation is tombstoned, not erased: it can be listed with
    // `conversations deleted` and brought back with `conversations restore`.
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
    // Handle /conversations restore <thread_id> - undo a soft delete (#684).
    if let Some(arg) = sub.strip_prefix("conversations restore ") {
        let arg = arg.trim();
        if arg.is_empty() {
            return cmd_display("Usage: /thread restore <n>");
        }
        // Accept a full id or an unambiguous prefix (the deleted list shows a
        // short id, and users paste that).
        let matches: Vec<String> = conv_mgr
            .list_deleted_conversations()
            .into_iter()
            .map(|(tid, _, _, _)| tid)
            .filter(|tid| tid.starts_with(arg))
            .collect();
        return match matches.len() {
            0 => cmd_display("No deleted conversation matches that id"),
            1 => {
                let tid = &matches[0];
                if conv_mgr.restore_thread(tid) {
                    serde_json::json!({
                        "display": format!(
                            "Restored conversation {}",
                            &tid[..8.min(tid.len())]
                        ),
                        "restored_thread_id": tid,
                    })
                } else {
                    cmd_display("Failed to restore conversation")
                }
            }
            n => cmd_display(format!("Ambiguous id: {} deleted conversations match", n)),
        };
    }
    // Handle /conversations deleted [N] - list soft-deleted conversations (#684).
    if sub == "conversations deleted" || sub.starts_with("conversations deleted ") {
        let limit = sub
            .strip_prefix("conversations deleted")
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .and_then(|a| a.parse::<usize>().ok())
            .unwrap_or(20);
        return format_deleted_conversations_json(conv_mgr, limit);
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
    // Handle /compact (full, issue #687) and /compact mini (truncate bulky
    // tool_result blocks, issue #641). Payload forms: `compact full:<tid>` and
    // `compact:<tid>`; the latter predates #687 and stays the mini form so
    // older clients keep their behavior.
    let compact_req = sub
        .strip_prefix("compact full:")
        .map(|tid| (tid, true))
        .or_else(|| sub.strip_prefix("compact:").map(|tid| (tid, false)));
    if let Some((tid, full)) = compact_req {
        if tid.is_empty() {
            return cmd_display("Error: missing thread_id");
        }
        if !conv_mgr.thread_exists(tid) {
            return cmd_display("Error: thread not found");
        }
        // Refuse while an agent loop is parked waiting for client tool
        // results; compacting now would orphan its in-flight work.
        {
            let loops = ctx.pending_loops.lock().await;
            if loops.values().any(|s| s.cm.thread_id == tid) {
                return cmd_display("Error: thread is waiting for tool results, try again later");
            }
        }
        // Invalidate any in-flight agent loop (e.g. awaiting an LLM response)
        // so it discards its work instead of appending to the rewritten file.
        bump_generation(&ctx.thread_generations, tid).await;
        if full {
            let backend = thread_backend(llm_backend, conv_mgr, tid);
            return match full_compact_thread(tid, &req.session_id, ctx, &backend, CompactTrigger::Command, None).await {
                Ok(Some(outcome)) => cmd_display(format_full_compact(&outcome)),
                Ok(None) => cmd_display("nothing to compact"),
                Err(e) => cmd_display(format!("Error: {}", e)),
            };
        }
        return match conv_mgr.compact_thread(tid) {
            Ok(Some(stats)) => cmd_display(format!(
                "compacted {} tool result(s), dropped {} old thinking block(s) ({} chars)\ncontext: {} -> {} chars (-{:.0}%)",
                stats.blocks,
                stats.thinking_blocks,
                format_count(stats.thinking_chars as u64),
                format_count(stats.total_before as u64),
                format_count(stats.total_after as u64),
                shrink_pct(stats.total_before, stats.total_after)
            )),
            Ok(None) => cmd_display("nothing to compact"),
            Err(e) => cmd_display(format!("Error: {}", e)),
        };
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
        s if s == "conversations stats" || s.starts_with("conversations stats ") => {
            let limit = s
                .strip_prefix("conversations stats")
                .map(|a| a.trim())
                .filter(|a| !a.is_empty())
                .and_then(|a| a.parse::<usize>().ok())
                .unwrap_or(10);
            format_thread_stats(conv_mgr, active_threads, &req.session_id, limit).await
        }
        s if s == "conversations search" || s.starts_with("conversations search ") => {
            let desc = s
                .strip_prefix("conversations search")
                .map(|a| a.trim())
                .unwrap_or("");
            handle_conversations_search(conv_mgr, active_threads, llm_backend, desc).await
        }
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
            cmd_display(command_query_tool.list_history(count, None))
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

/// Render a hostname for the `/resume all` listing. Strips the macOS
/// `.local` mDNS suffix and truncates to 12 chars (with `..` ellipsis)
/// so the host column doesn't crowd out the title and last-question
/// columns. Missing or empty host (legacy threads) renders as `?`.
fn short_host(host: Option<&str>) -> String {
    let Some(h) = host.map(str::trim).filter(|s| !s.is_empty()) else {
        return "?".to_string();
    };
    let h = h.strip_suffix(".local").unwrap_or(h);
    const MAX: usize = 12;
    if h.chars().count() <= MAX {
        h.to_string()
    } else {
        let head: String = h.chars().take(MAX - 2).collect();
        format!("{}..", head)
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
                // Compare normalized: macOS gethostname() flips between `X` and
                // `X.local` across a network drop/reconnect, and an exact match
                // then hid every conversation from /resume (issue #683).
                conv_mgr
                    .load_meta(tid)
                    .host
                    .as_deref()
                    .is_some_and(|h| omnish_common::host::hosts_match(h, host))
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

    // Cross-host mode (`/resume all`) inserts the originating host between
    // time and turns so users can scan which machine each thread came from.
    // Single-host mode (default `/resume`) skips it: the filter already
    // guarantees every line shares the current host, so the column would
    // be redundant clutter.
    let show_host = host_filter.is_none();

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

        let host_segment = if show_host {
            format!("@{} | ", short_host(meta.host.as_deref()))
        } else {
            String::new()
        };

        if let Some(ref title) = meta.summary {
            output.push_str(&format!(
                "  [{}] {} | {}{} turns | {} | {}\n",
                i + 1,
                time_ago,
                host_segment,
                exchange_count,
                truncate_display(title, 30),
                truncate_display(&last_question, 30),
            ));
        } else {
            output.push_str(&format!(
                "  [{}] {} | {}{} turns | {}\n",
                i + 1,
                time_ago,
                host_segment,
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

/// Format soft-deleted conversations for `/thread deleted` (issue #684).
/// Mirrors `format_conversations_json`'s shape so the client can reuse the
/// same display/thread_ids handling.
fn format_deleted_conversations_json(
    conv_mgr: &Arc<ConversationManager>,
    limit: usize,
) -> serde_json::Value {
    let all = conv_mgr.list_deleted_conversations();
    if all.is_empty() {
        return cmd_display("No deleted conversations.");
    }
    let total = all.len();
    let shown: Vec<_> = all.into_iter().take(limit).collect();

    let mut output = String::from("Deleted conversations:\n");
    let mut thread_ids = Vec::new();
    for (i, (thread_id, deleted_at, exchange_count, last_question)) in
        shown.into_iter().enumerate()
    {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_millis(deleted_at);
        let single_line = last_question.replace('\n', " ");
        let question: String = if single_line.chars().count() > 40 {
            format!("{}...", single_line.chars().take(37).collect::<String>())
        } else {
            single_line
        };
        output.push_str(&format!(
            "  [{}] {} | deleted {} | {} turns | {}\n",
            i + 1,
            &thread_id[..8.min(thread_id.len())],
            format_relative_time(when),
            exchange_count,
            question,
        ));
        thread_ids.push(thread_id);
    }
    if total > thread_ids.len() {
        output.push_str(&format!(
            "  ({} total, showing {}. Use /thread deleted N to see more)\n",
            total,
            thread_ids.len()
        ));
    }
    output.push_str("  Restore with: /thread restore <id>\n");
    serde_json::json!({
        "display": output,
        "thread_ids": thread_ids,
    })
}

/// Bound a user question for the `/resume ?` prompt: keep at most 3 lines,
/// each capped at 200 chars, eliding the remainder with "..." so a large
/// pasted block cannot dominate the payload.
fn truncate_question(q: &str) -> String {
    let mut lines: Vec<String> = q
        .lines()
        .take(3)
        .map(|l| {
            if l.chars().count() > 200 {
                let s: String = l.chars().take(197).collect();
                format!("{}...", s)
            } else {
                l.to_string()
            }
        })
        .collect();
    if q.lines().count() > 3 {
        lines.push("...".to_string());
    }
    lines.join("\n  ")
}

/// Soft cap on the character count of a single `/resume ?` search request.
/// Larger thread sets are split into several requests and queried
/// concurrently (issue #685).
const RESUME_SEARCH_MAX_REQUEST_CHARS: usize = 50_000;

/// Cap on the search description the user types after `/resume ?`. The text is
/// repeated in every chunked request, so an unbounded paste would shrink the
/// budget left for thread content (and in the extreme drive the chunk count up).
const RESUME_SEARCH_MAX_DESCRIPTION_CHARS: usize = 500;

/// Truncate to at most `max` characters, marking that content was dropped.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}... (truncated)")
}

/// Split threads into request bodies, keeping every thread whole (issue #685).
///
/// `budget` is a **soft** limit: threads are appended to the current body until
/// adding the next one would exceed it, at which point a new body starts. A
/// thread is never split, so every request sees a thread's complete question
/// list. That matters because each request scores independently: a thread split
/// across two requests would be judged twice from partial views, and the merge
/// would have to guess which partial score to trust.
///
/// The cost of never splitting is that a single thread larger than `budget`
/// gets a body of its own that exceeds it (the backend's `max_content_chars`
/// is the backstop there).
fn build_search_chunks(threads: &[(String, Vec<String>)], budget: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for (header, questions) in threads {
        let mut block = String::with_capacity(
            header.len() + questions.iter().map(|q| q.len()).sum::<usize>() + 1,
        );
        block.push_str(header);
        for q in questions {
            block.push_str(q);
        }
        // Blank line between threads, matching the unsplit prompt layout.
        block.push('\n');

        if !current.is_empty() && current.chars().count() + block.chars().count() > budget {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(&block);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Parse the LLM's `/resume ?` search reply into `(thread_id, reason)` pairs.
/// Keeps the LLM's order, drops unknown thread_ids and duplicates. Any
/// malformed output yields an empty result (treated as "no match").
/// Relevance used when a reply omits `score` or gives an unparsable one.
/// Mid-scale so a missing score neither jumps to the top nor sinks below real
/// matches; ties fall back to the surrounding order (recency).
const RESUME_SEARCH_DEFAULT_SCORE: f32 = 0.5;

/// Coerce the LLM's `score` field to `[0,1]`. Accepts a number or a numeric
/// string: models sometimes quote it, and a strict type here would fail the
/// whole array and silently drop every match in that chunk.
fn parse_relevance_score(v: &serde_json::Value) -> f32 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        .filter(|f| f.is_finite())
        .map(|f| f as f32)
        .unwrap_or(RESUME_SEARCH_DEFAULT_SCORE)
        .clamp(0.0, 1.0)
}

fn parse_resume_search(
    content: &str,
    valid: &std::collections::HashSet<String>,
) -> Vec<(String, String, f32)> {
    let Some(json_str) = extract_first_json_array(content.trim()) else {
        return Vec::new();
    };

    #[derive(serde::Deserialize)]
    struct Raw {
        thread_id: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        score: serde_json::Value,
    }

    let Ok(raw) = serde_json::from_str::<Vec<Raw>>(json_str) else {
        return Vec::new();
    };

    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter(|r| valid.contains(&r.thread_id) && seen.insert(r.thread_id.clone()))
        .map(|r| {
            let score = parse_relevance_score(&r.score);
            (r.thread_id, r.reason, score)
        })
        .collect()
}

/// Handle `__cmd:conversations search <description>` (the `/resume ?` feature):
/// ask the LLM which of all threads (all hosts) match the description, and
/// return the matches (LLM order) in the same envelope the picker consumes.
async fn handle_conversations_search(
    conv_mgr: &Arc<ConversationManager>,
    active_threads: &ActiveThreads,
    llm_backend: &Arc<MultiBackend>,
    description: &str,
) -> serde_json::Value {
    if description.is_empty() {
        return cmd_display("Please provide a description to search for.");
    }
    // Bound the user's description: it is repeated verbatim in every chunked
    // request, so a pasted blob would both inflate each request and shrink the
    // per-request budget for actual thread content.
    let description = &truncate_chars(description, RESUME_SEARCH_MAX_DESCRIPTION_CHARS);

    let conversations = conv_mgr.list_conversations();
    if conversations.is_empty() {
        return cmd_status("No conversations yet. Start a chat with :", "no_conversations");
    }

    // Build per-thread blocks: each thread's id, title, and the user's questions.
    let mut thread_entries: Vec<(String, Vec<String>)> = Vec::new();
    let mut valid: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (tid, _modified, _count, _last_q) in &conversations {
        let meta = conv_mgr.load_meta(tid);
        let title = meta.summary.as_deref().unwrap_or("untitled");
        let header = format!("[{}] {}\n", tid, title);
        let mut questions: Vec<String> = Vec::new();
        for (user_q, _assistant) in conv_mgr.get_all_exchanges(tid) {
            let q = user_q.trim();
            if q.is_empty() {
                continue;
            }
            // Keep questions readable but bound pasted blobs: at most 3 lines
            // (each capped), rest elided with "...".
            questions.push(format!("  Q: {}\n", truncate_question(q)));
        }
        thread_entries.push((header, questions));
        valid.insert(tid.clone());
    }

    let query = format!(
        "{}\n\nThe user is looking for: {}",
        omnish_llm::template::RESUME_SEARCH_PROMPT, description
    );
    // Split oversized thread sets into several requests (issue #685). The
    // budget covers the thread bodies; wrapper + query are subtracted so a
    // request's total stays within the cap.
    let wrapper_chars = "<threads>\n".chars().count() + "\n</threads>".chars().count();
    let budget = RESUME_SEARCH_MAX_REQUEST_CHARS
        .saturating_sub(wrapper_chars + query.chars().count())
        .max(1);
    let bodies = build_search_chunks(&thread_entries, budget);
    let total_requests = bodies.len();
    tracing::info!(
        "resume search: {} thread(s) in {} request(s), cap {} chars/request",
        thread_entries.len(),
        total_requests,
        RESUME_SEARCH_MAX_REQUEST_CHARS
    );

    // Hand the chunks to the daemon-wide LLM pool: it bounds how many requests
    // are in flight at once (see llm_pool::MAX_CONCURRENT_LLM_CALLS) and returns
    // outcomes in request order.
    let max_content_chars = llm_backend.get_max_content_chars(UseCase::Summarize);
    let requests: Vec<LlmRequest> = bodies
        .into_iter()
        .enumerate()
        .map(|(idx, body)| {
            let context = format!("<threads>\n{}</threads>", body);
            tracing::info!(
                "resume search: request {}/{}: {} chars, {} bytes",
                idx + 1,
                total_requests,
                context.chars().count() + query.chars().count(),
                context.len() + query.len()
            );
            LlmRequest {
                context,
                query: Some(query.clone()),
                trigger: TriggerType::Manual,
                session_ids: vec![],
                use_case: UseCase::Summarize,
                max_content_chars,
                system_prompt: None,
                enable_thinking: Some(false),
                tools: vec![],
                extra_messages: vec![],
            }
        })
        .collect();

    let outcomes = omnish_daemon::llm_pool::complete_all(
        llm_backend,
        requests,
        std::time::Duration::from_secs(45),
    )
    .await;

    // Reassemble in request order (chunks follow the newest-first thread order)
    // and drop cross-chunk duplicates.
    let mut any_timeout = false;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut matched: Vec<(String, String, f32)> = Vec::new();
    for (idx, outcome) in outcomes.iter().enumerate() {
        match outcome {
            omnish_daemon::llm_pool::LlmOutcome::Ok(resp) => {
                for (tid, reason, score) in parse_resume_search(&resp.text(), &valid) {
                    if seen.insert(tid.clone()) {
                        matched.push((tid, reason, score));
                    }
                }
            }
            omnish_daemon::llm_pool::LlmOutcome::Failed(e) => {
                tracing::warn!("resume search request {} failed: {}", idx + 1, e);
            }
            omnish_daemon::llm_pool::LlmOutcome::TimedOut => {
                any_timeout = true;
                tracing::warn!("resume search request {} timed out after 45s", idx + 1);
            }
        }
    }

    // Each chunk ranks only the threads it saw, so concatenating them would
    // order results by chunk (i.e. by recency) rather than by relevance. The
    // prompt asks for an absolute 0-1 score judged independently of the other
    // threads in the chunk, which makes scores comparable across requests;
    // sort on it to restore a global ranking. The sort is stable, so equal
    // scores keep the previous order (chunk order = newest first, and the
    // LLM's own ranking within a chunk).
    matched.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    if matched.is_empty() {
        if any_timeout {
            return cmd_status("Conversation search timed out. Please try again.", "timeout");
        }
        return cmd_status("No matching conversation found.", "no_match");
    }

    let locked_set: std::collections::HashSet<String> = {
        let threads = active_threads.lock().await;
        threads.keys().cloned().collect()
    };

    let truncate = |s: &str, max: usize| -> String {
        let one = s.replace('\n', " ");
        if one.chars().count() > max {
            let end: String = one.chars().take(max - 3).collect();
            format!("{}...", end)
        } else {
            one
        }
    };

    let mut output = String::from("Matching conversations:\n");
    let mut thread_ids = Vec::new();
    let mut locked_threads = Vec::new();
    for (i, (tid, reason, _score)) in matched.iter().enumerate() {
        let meta = conv_mgr.load_meta(tid);
        let (modified, turns) = conversations
            .iter()
            .find(|(t, _, _, _)| t == tid)
            .map(|(_, m, c, _)| (*m, *c))
            .unwrap_or((std::time::SystemTime::UNIX_EPOCH, 0));
        let time_ago = format_relative_time(modified);
        let title = meta.summary.as_deref().unwrap_or("untitled");
        output.push_str(&format!(
            "  [{}] {} | @{} | {} turns | {} | {}\n",
            i + 1,
            time_ago,
            short_host(meta.host.as_deref()),
            turns,
            truncate(title, 30),
            truncate(reason, 40),
        ));
        thread_ids.push(tid.clone());
        locked_threads.push(locked_set.contains(tid));
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
///
/// `limit` caps the number of threads shown when no active thread is bound to
/// this session. With an active thread, only that one is shown and the limit
/// is irrelevant.
async fn format_thread_stats(conv_mgr: &Arc<ConversationManager>, active_threads: &ActiveThreads, session_id: &str, limit: usize) -> serde_json::Value {
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
                format_count(last.input_tokens + last.cache_read_input_tokens + last.cache_creation_input_tokens + last.output_tokens),
                format_count(total.input_tokens + total.cache_read_input_tokens + total.cache_creation_input_tokens + total.output_tokens),
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

    // No active thread: fall back to showing the most recent `limit` threads.
    let conversations = conv_mgr.list_conversations();
    if conversations.is_empty() {
        return cmd_display("No conversations yet. Start a chat with :");
    }

    let locked_set: std::collections::HashSet<String> = {
        let threads = active_threads.lock().await;
        threads.keys().cloned().collect()
    };

    let total = conversations.len();
    let shown: Vec<_> = conversations.into_iter().take(limit).collect();

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

    let shown_count = shown.len();
    for (i, (thread_id, modified, exchange_count, _last_question)) in shown.into_iter().enumerate() {
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
                format_count(last.input_tokens + last.cache_read_input_tokens + last.cache_creation_input_tokens + last.output_tokens),
                format_count(total.input_tokens + total.cache_read_input_tokens + total.cache_creation_input_tokens + total.output_tokens),
                cache_rate,
            ));
        } else {
            output.push_str("       (no usage data)\n");
        }
        if meta.sandbox_disabled == Some(true) {
            output.push_str("       sandbox: off\n");
        }
    }
    if total > shown_count {
        output.push_str(&format!(
            "  ({} total, showing {}. Use /thread stats N to see more)\n",
            total, shown_count,
        ));
    }
    cmd_display(output)
}

/// Format a count (tokens or chars) with K/M suffix for readability.
fn format_count(n: u64) -> String {
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
/// `hourly_since` is the cron-derived window start for the hourly preview.
async fn handle_context_scenario(scenario: &str, req: &Request, mgr: &SessionManager, llm_backend: &Arc<MultiBackend>, conv_mgr: &Arc<ConversationManager>, hourly_since: chrono::DateTime<chrono::Local>) -> String {
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
            let since_ms = hourly_since.timestamp_millis().max(0) as u64;
            let commands = mgr.collect_recent_commands(since_ms).await;
            let conversations_md = conv_mgr.collect_recent_conversations_md(hourly_since.into());
            if commands.is_empty() && conversations_md.is_empty() {
                return format!(
                    "No commands or conversations since {}",
                    hourly_since.format("%Y-%m-%d %H:%M")
                );
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
    let result = omnish_daemon::llm_pool::complete(backend.as_ref(), &llm_req).await;
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

    // Resolve current cwd: prefer client-reported (more accurate at request
    // time), fall back to polled shell_cwd attribute on the session.
    let cwd = match req.cwd.as_deref() {
        Some(c) => Some(omnish_context::shorten_home(c)),
        None => mgr.get_live_cwd(&req.session_id).await,
    };
    let cwd_query = cwd.as_deref().map(|c| omnish_daemon::session_mgr::CwdQuery {
        cwd: c,
        prefix: req.input.as_str(),
    });

    let sections = mgr
        .build_completion_sections(&req.session_id, max_context_chars, cwd_query)
        .await?;
    // Stable portion for prefix-ratio comparison (mirrors what
    // `check_and_warmup_sections` caches into `last_completion_context`).
    let stable_context = if sections.stable_prefix.is_empty() && sections.remainder.is_empty() {
        String::new()
    } else {
        format!("{}{}", sections.stable_prefix, sections.remainder)
    };
    let context = if stable_context.is_empty() && sections.cwd_history.is_empty() {
        String::new()
    } else {
        format!("{}{}", stable_context, sections.cwd_history)
    };

    // Log prefix match ratio with previous completion request.
    // Compare on the stable portion only - cwd_history depends on per-request
    // input and would skew the ratio without informing KV-cache stability.
    if !last_context.is_empty() {
        let common_prefix_len = last_context
            .bytes()
            .zip(stable_context.bytes())
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
    let result = omnish_daemon::llm_pool::complete(backend.as_ref(), &llm_req).await;
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

/// Take the first line of an LLM completion candidate. Ghost text is rendered
/// on a single line, so any embedded newline is a defect: the LLM sometimes
/// glues two candidates together with `\n` (issue #654) or appends a trailing
/// newline (issue #652). Truncating at the first `\n`/`\r` handles both.
fn first_line(s: &str) -> String {
    s.split(['\n', '\r']).next().unwrap_or("").to_string()
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
            // Keep only the first line; the LLM sometimes joins two candidates
            // with `\n` (issue #654) or appends a trailing newline (issue #652),
            // and an embedded \n breaks ghost-text insertion.
            .map(|s| first_line(&s))
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
                // Keep only the first line (issues #652, #654).
                text: first_line(&r.text),
                confidence: r.confidence.clamp(0.0, 1.0),
            })
            .filter(|s| !s.text.is_empty())
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

    fn tool_call(name: &str, input: serde_json::Value) -> omnish_llm::tool::ToolCall {
        omnish_llm::tool::ToolCall { id: "id".into(), name: name.into(), input, extra: Default::default() }
    }

    #[test]
    fn test_tool_expected_duration() {
        use std::time::Duration;
        // bash timeout is milliseconds; default 5m, hard-capped at 15m.
        assert_eq!(tool_expected_duration("bash", &serde_json::json!({})), Duration::from_secs(300));
        assert_eq!(tool_expected_duration("bash", &serde_json::json!({"timeout": 600000})), Duration::from_secs(600));
        assert_eq!(tool_expected_duration("bash", &serde_json::json!({"timeout": 9_999_999})), Duration::from_secs(900));
        // grep timeout is seconds; default 60s, clamped to 300s.
        assert_eq!(tool_expected_duration("grep", &serde_json::json!({})), Duration::from_secs(60));
        assert_eq!(tool_expected_duration("grep", &serde_json::json!({"timeout": 9999})), Duration::from_secs(300));
        // tools without a timeout param contribute nothing.
        assert_eq!(tool_expected_duration("read", &serde_json::json!({})), Duration::ZERO);
    }

    #[test]
    fn test_parked_loop_idle_deadline() {
        // No pending tools (or short ones): stays at the base window.
        assert_eq!(parked_loop_idle_deadline(&[]), PARKED_LOOP_BASE_IDLE);
        assert_eq!(
            parked_loop_idle_deadline(&[tool_call("bash", serde_json::json!({"timeout": 600000}))]),
            PARKED_LOOP_BASE_IDLE,
        );
        // bash caps at 900s; 900s + 60s margin = 960s < 1800s base, so still base.
        // (No current tool can declare a timeout beyond the base window, so the
        // auto-extension is future-proofing rather than active behavior today.)
        let capped_bash = tool_call("bash", serde_json::json!({"timeout": 9_999_999}));
        assert_eq!(parked_loop_idle_deadline(std::slice::from_ref(&capped_bash)), PARKED_LOOP_BASE_IDLE);
        // The deadline is the max across all pending tools.
        let mixed = vec![
            tool_call("read", serde_json::json!({})),
            tool_call("grep", serde_json::json!({"timeout": 300})),
        ];
        assert_eq!(parked_loop_idle_deadline(&mixed), PARKED_LOOP_BASE_IDLE);
    }

    #[test]
    fn test_parse_omnish_debug_args() {
        // Empty: defaults
        assert_eq!(parse_omnish_debug_args(""), (None, None));

        // Legacy bare number: length only
        assert_eq!(parse_omnish_debug_args("60"), (None, Some(60)));
        assert_eq!(parse_omnish_debug_args("0"), (None, Some(0)));

        // Delay only
        assert_eq!(parse_omnish_debug_args("delay 500"), (Some(500), None));

        // Length keyword (and short form)
        assert_eq!(parse_omnish_debug_args("length 60"), (None, Some(60)));
        assert_eq!(parse_omnish_debug_args("len 60"), (None, Some(60)));

        // Combined, both orders
        assert_eq!(
            parse_omnish_debug_args("delay 500 length 60"),
            (Some(500), Some(60))
        );
        assert_eq!(
            parse_omnish_debug_args("length 60 delay 500"),
            (Some(500), Some(60))
        );

        // Mixed bare-number + delay (length wins via keyword, bare ignored
        // once length is set)
        assert_eq!(
            parse_omnish_debug_args("delay 200 60"),
            (Some(200), Some(60))
        );

        // Bare-number only set once; subsequent ones ignored
        assert_eq!(parse_omnish_debug_args("60 80"), (None, Some(60)));

        // Unknown tokens are silently skipped
        assert_eq!(
            parse_omnish_debug_args("foo delay 100 bar length 30 baz"),
            (Some(100), Some(30))
        );

        // Missing values after keyword: keyword absorbed, default kept
        assert_eq!(parse_omnish_debug_args("delay"), (None, None));
        assert_eq!(parse_omnish_debug_args("length"), (None, None));
    }

    #[test]
    fn test_short_host() {
        // Missing / empty host (legacy threads): renders as "?".
        assert_eq!(short_host(None), "?");
        assert_eq!(short_host(Some("")), "?");
        assert_eq!(short_host(Some("   ")), "?");

        // Plain host under the limit: rendered as-is.
        assert_eq!(short_host(Some("box1")), "box1");
        assert_eq!(short_host(Some("workstation1")), "workstation1");

        // .local mDNS suffix stripped (matches the resume-mismatch fix in #600).
        assert_eq!(short_host(Some("box1.local")), "box1");

        // Over the 12-char limit: head + ".." (10 + 2 = 12).
        assert_eq!(short_host(Some("really-long-hostname")), "really-lon..");
    }

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
    fn test_build_completion_extra_messages_inserts_cwd_history_block() {
        let sections = omnish_context::recent::CompletionSections {
            stable_prefix: "<history>S</history>".into(),
            remainder: "<system-reminder>R</system-reminder>".into(),
            cwd_history: "<cwd_history>\ngit status\n</cwd_history>".into(),
        };
        let msgs = build_completion_extra_messages(&sections, "Current input: `git`");
        assert_eq!(msgs.len(), 1);
        let blocks = msgs[0].content["content"].as_array().unwrap();
        // Order: stable_prefix, remainder, cwd_history, query
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0]["text"], "<history>S</history>");
        assert_eq!(blocks[1]["text"], "<system-reminder>R</system-reminder>");
        assert_eq!(blocks[2]["text"], "<cwd_history>\ngit status\n</cwd_history>");
        assert_eq!(blocks[3]["text"], "Current input: `git`");
        // Cache hint sits on block 0 (the stable_prefix).
        assert_eq!(msgs[0].cache_pos, Some(0));
    }

    #[test]
    fn test_build_completion_extra_messages_skips_empty_cwd_history() {
        let sections = omnish_context::recent::CompletionSections {
            stable_prefix: "S".into(),
            remainder: "R".into(),
            cwd_history: String::new(),
        };
        let msgs = build_completion_extra_messages(&sections, "q");
        let blocks = msgs[0].content["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 3); // no cwd_history block when empty
        assert_eq!(blocks[0]["text"], "S");
        assert_eq!(blocks[1]["text"], "R");
        assert_eq!(blocks[2]["text"], "q");
    }

    fn resp(content: Vec<ContentBlock>, stop_reason: StopReason) -> LlmResponse {
        LlmResponse { content, stop_reason, model: "m".to_string(), usage: None }
    }

    fn tool_use_block(id: &str) -> ContentBlock {
        ContentBlock::ToolUse(omnish_llm::tool::ToolCall {
            id: id.to_string(),
            name: "read".to_string(),
            input: serde_json::json!({}),
            extra: Default::default(),
        })
    }

    #[test]
    fn test_should_execute_tools_normal_tool_use() {
        let r = resp(vec![tool_use_block("a")], StopReason::ToolUse);
        assert!(should_execute_tools(&r));
    }

    #[test]
    fn test_should_execute_tools_mislabeled_end_turn() {
        // Tool calls present but stop_reason wrongly reported as end_turn:
        // still execute (the fix for dropped/orphaned tool calls).
        let r = resp(
            vec![ContentBlock::Text("ok".to_string()), tool_use_block("a")],
            StopReason::EndTurn,
        );
        assert!(should_execute_tools(&r));
    }

    #[test]
    fn test_should_execute_tools_max_tokens_is_truncation() {
        // Truncated response may end on an incomplete tool_use; treat as
        // truncation, do NOT execute.
        let r = resp(vec![tool_use_block("a")], StopReason::MaxTokens);
        assert!(!should_execute_tools(&r));
    }

    #[test]
    fn test_should_execute_tools_no_tool_calls() {
        let r = resp(vec![ContentBlock::Text("done".to_string())], StopReason::EndTurn);
        assert!(!should_execute_tools(&r));
        // Degenerate: stop_reason ToolUse but no tool blocks -> not executed
        // (falls through to the final path instead of the old dead `break`).
        let r2 = resp(vec![ContentBlock::Text("x".to_string())], StopReason::ToolUse);
        assert!(!should_execute_tools(&r2));
    }

    #[test]
    fn test_build_empty_response_message_preserves_thinking() {
        // A thinking-only turn: the reasoning must be recorded, with the marker
        // appended as the visible text output.
        let content = vec![ContentBlock::Thinking {
            thinking: "reasoned about the cleanup".to_string(),
            signature: Some("sig-1".to_string()),
        }];
        let msg = build_empty_response_message(&content, "<event>empty response</event>");
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().expect("content should be an array");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["thinking"], "reasoned about the cleanup");
        assert_eq!(blocks[0]["signature"], "sig-1");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "<event>empty response</event>");
    }

    #[test]
    fn test_build_empty_response_message_drops_tool_use() {
        // tool_use blocks must not be persisted here (no matching tool_result
        // would follow), but thinking is still preserved.
        let content = vec![
            ContentBlock::Thinking { thinking: "t".to_string(), signature: None },
            ContentBlock::ToolUse(omnish_llm::tool::ToolCall {
                id: "id1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({}),
                extra: Default::default(),
            }),
        ];
        let msg = build_empty_response_message(&content, "<event>empty response</event>");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| b["type"] != "tool_use"));
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[1]["type"], "text");
    }

    #[test]
    fn test_build_empty_response_message_no_thinking_is_bare_marker() {
        // No thinking: fall back to the bare marker string (existing behavior).
        let msg = build_empty_response_message(&[], "<event>empty response</event>");
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "<event>empty response</event>");
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
    fn test_truncate_question() {
        // Short single line unchanged.
        assert_eq!(truncate_question("deploy the app"), "deploy the app");
        // More than 3 lines -> first 3 kept, rest elided.
        let q = "line1\nline2\nline3\nline4\nline5";
        assert_eq!(truncate_question(q), "line1\n  line2\n  line3\n  ...");
        // Over-long single line capped at 200 chars with ellipsis.
        let long = "x".repeat(500);
        let out = truncate_question(&long);
        assert_eq!(out.chars().count(), 200);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn test_parse_resume_search() {
        let valid: std::collections::HashSet<String> =
            ["t1", "t2", "t3"].iter().map(|s| s.to_string()).collect();

        // Relevant subset returned in LLM order, unknown id dropped.
        let content = r#"prose [{"thread_id":"t2","reason":"deploy pipeline"},{"thread_id":"t1","reason":"docker build"},{"thread_id":"nope","reason":"x"}]"#;
        let result = parse_resume_search(content, &valid);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "t2");
        assert_eq!(result[0].1, "deploy pipeline");
        assert_eq!(result[1].0, "t1");
        assert_eq!(result[1].1, "docker build");

        // Duplicate thread_id kept once (first occurrence).
        let dup = r#"[{"thread_id":"t1","reason":"a"},{"thread_id":"t1","reason":"b"}]"#;
        let result = parse_resume_search(dup, &valid);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "t1");
        assert_eq!(result[0].1, "a");

        // Missing reason defaults to empty.
        let no_reason = r#"[{"thread_id":"t3"}]"#;
        let result = parse_resume_search(no_reason, &valid);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "t3");
        assert_eq!(result[0].1, "");

        // Empty array and malformed output -> no matches.
        assert!(parse_resume_search("[]", &valid).is_empty());
        assert!(parse_resume_search("not json", &valid).is_empty());
        assert!(parse_resume_search(r#"{"thread_id":"t1"}"#, &valid).is_empty());
    }

    /// The relevance score is what restores a global ranking once a search is
    /// split into several independent requests (issue #685).
    #[test]
    fn test_parse_resume_search_scores() {
        let valid: std::collections::HashSet<String> =
            ["t1", "t2", "t3", "t4", "t5"].iter().map(|s| s.to_string()).collect();

        let content = r#"[
            {"thread_id":"t1","score":0.9,"reason":"a"},
            {"thread_id":"t2","score":"0.4","reason":"quoted number"},
            {"thread_id":"t3","reason":"no score"},
            {"thread_id":"t4","score":7,"reason":"out of range"},
            {"thread_id":"t5","score":"abc","reason":"garbage"}
        ]"#;
        let result = parse_resume_search(content, &valid);
        assert_eq!(result.len(), 5, "one bad score must not drop the whole array");

        let score = |tid: &str| result.iter().find(|r| r.0 == tid).unwrap().2;
        assert!((score("t1") - 0.9).abs() < 1e-6);
        // A quoted number is still a number: a strict type here would have
        // failed the array and silently lost every match in the chunk.
        assert!((score("t2") - 0.4).abs() < 1e-6);
        // Missing / unparsable fall back to mid-scale, not to 0 or 1.
        assert!((score("t3") - RESUME_SEARCH_DEFAULT_SCORE).abs() < 1e-6);
        assert!((score("t5") - RESUME_SEARCH_DEFAULT_SCORE).abs() < 1e-6);
        // Out of range is clamped.
        assert!((score("t4") - 1.0).abs() < 1e-6);
    }

    /// Merging chunk results must order by score, not by which chunk replied
    /// first, and must keep the earlier order when scores tie (issue #685).
    #[test]
    fn test_merged_matches_sort_by_score_stably() {
        // As produced by the merge loop: chunk 0 first, then chunk 1.
        let mut matched: Vec<(String, String, f32)> = vec![
            ("newest-weak".into(), "chunk0".into(), 0.3),
            ("newest-tie-a".into(), "chunk0".into(), 0.6),
            ("older-strong".into(), "chunk1".into(), 0.95),
            ("older-tie-b".into(), "chunk1".into(), 0.6),
        ];
        matched.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let order: Vec<&str> = matched.iter().map(|m| m.0.as_str()).collect();
        // The strong match from a later chunk now outranks the weak newest one.
        assert_eq!(
            order,
            vec!["older-strong", "newest-tie-a", "older-tie-b", "newest-weak"]
        );
    }

    /// Issue #685: the user's search description is capped so it cannot
    /// inflate every chunked request (and shrink the budget for thread text).
    #[test]
    fn test_truncate_chars_caps_description() {
        assert_eq!(truncate_chars("short", 500), "short");
        // Exactly at the cap is left alone.
        let exact = "x".repeat(RESUME_SEARCH_MAX_DESCRIPTION_CHARS);
        assert_eq!(truncate_chars(&exact, RESUME_SEARCH_MAX_DESCRIPTION_CHARS), exact);

        let long = "y".repeat(5_000);
        let cut = truncate_chars(&long, RESUME_SEARCH_MAX_DESCRIPTION_CHARS);
        assert!(cut.starts_with(&"y".repeat(RESUME_SEARCH_MAX_DESCRIPTION_CHARS)));
        assert!(cut.ends_with("... (truncated)"), "got {cut}");
        // Bounded regardless of input size.
        assert!(cut.chars().count() < RESUME_SEARCH_MAX_DESCRIPTION_CHARS + 32);

        // Multi-byte input is cut on char boundaries, not bytes.
        let cjk = "\u{4e2d}".repeat(1_000);
        let cut = truncate_chars(&cjk, 10);
        assert!(cut.starts_with(&"\u{4e2d}".repeat(10)));
        assert_eq!(cut.chars().filter(|c| *c == '\u{4e2d}').count(), 10);
    }

    /// Issue #685: oversized searches are split into several requests, but a
    /// thread is never split across them: each request must see a thread's
    /// complete question list so it can score it from a full view.
    #[test]
    fn test_build_search_chunks_keeps_threads_whole() {
        let threads: Vec<(String, Vec<String>)> = (0..6)
            .map(|i| {
                (
                    format!("[t{}] title\n", i),
                    vec![format!("  Q: aaa{}\n", i), format!("  Q: bbb{}\n", i)],
                )
            })
            .collect();
        // Each thread block is ~11 + 2*10 + 1 chars, so a 60-char budget forces
        // a split but still fits more than one thread per chunk.
        let chunks = build_search_chunks(&threads, 60);
        assert!(chunks.len() >= 2, "expected a split, got {:?}", chunks);

        // Every thread appears exactly once, header and both questions together
        // in the same chunk.
        for i in 0..6 {
            let header = format!("[t{}] title", i);
            let owners: Vec<&String> = chunks.iter().filter(|c| c.contains(&header)).collect();
            assert_eq!(owners.len(), 1, "thread t{} spans {} chunks", i, owners.len());
            let owner = owners[0];
            assert!(owner.contains(&format!("  Q: aaa{}", i)), "t{} lost a question", i);
            assert!(owner.contains(&format!("  Q: bbb{}", i)), "t{} lost a question", i);
        }
    }

    /// The budget is soft: a single thread bigger than it is not split, it just
    /// gets a body of its own (issue #685).
    #[test]
    fn test_build_search_chunks_soft_limit_on_huge_thread() {
        let huge: Vec<String> = (0..50).map(|i| format!("  Q: long question {}\n", i)).collect();
        let threads = vec![
            ("[small] a\n".to_string(), vec!["  Q: x\n".to_string()]),
            ("[huge] b\n".to_string(), huge.clone()),
            ("[after] c\n".to_string(), vec!["  Q: y\n".to_string()]),
        ];
        let chunks = build_search_chunks(&threads, 50);

        let huge_chunks: Vec<&String> = chunks.iter().filter(|c| c.contains("[huge] b")).collect();
        assert_eq!(huge_chunks.len(), 1, "oversized thread must not be split");
        // All of its questions stayed together, even though that exceeds budget.
        for i in 0..50 {
            assert!(
                huge_chunks[0].contains(&format!("  Q: long question {}\n", i)),
                "missing question {}",
                i
            );
        }
        assert!(huge_chunks[0].chars().count() > 50, "expected soft overflow");
        // Neighbours are unaffected and still present exactly once.
        assert_eq!(chunks.iter().filter(|c| c.contains("[small] a")).count(), 1);
        assert_eq!(chunks.iter().filter(|c| c.contains("[after] c")).count(), 1);
    }

    #[test]
    fn test_build_search_chunks_single_when_fits() {
        let threads = vec![(
            "[t1] a\n".to_string(),
            vec!["  Q: x\n".to_string()],
        )];
        let chunks = build_search_chunks(&threads, 1000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("[t1] a"));
        assert!(chunks[0].contains("  Q: x"));
    }

    #[test]
    fn test_parse_completion_suggestions_strips_trailing_newline() {
        // Issue #652: LLM sometimes appends a trailing newline to a candidate.
        let input = "[\"git status\\n\", \"git stash\\r\\n\"]";
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "git status");
        assert_eq!(result[1].text, "git stash");

        // Object-array format is stripped too, and a newline-only candidate is dropped.
        let obj = "[{\"text\": \"ls -la\\n\", \"confidence\": 0.9}, {\"text\": \"\\n\", \"confidence\": 0.5}]";
        let result = parse_completion_suggestions(obj).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "ls -la");
    }

    #[test]
    fn test_parse_completion_suggestions_truncates_embedded_newline() {
        // Issue #654: LLM sometimes glues two candidates together with `\n`
        // inside a single string. Only the first line is kept.
        let input = "[\"scp a.tar.gz eagle4:~/\\nscp a.tar.gz eagle4:~/\"]";
        let result = parse_completion_suggestions(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "scp a.tar.gz eagle4:~/");

        // Object-array format truncates embedded newlines too.
        let obj = "[{\"text\": \"git commit\\ngit push\", \"confidence\": 0.9}]";
        let result = parse_completion_suggestions(obj).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "git commit");
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
        mgr.register("test_session", None, std::collections::HashMap::new(), None)
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
        mgr.register("session_a", None, std::collections::HashMap::new(), None)
            .await
            .unwrap();
        mgr.register("session_b", None, std::collections::HashMap::new(), None)
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

    /// Regression for #672: resuming a thread and choosing "cd to the thread's
    /// original directory" must NOT overwrite the thread's stored cwd with the
    /// transient resume-time cwd. touch_thread records the session's CURRENT
    /// cwd (updated by the post-resume SessionUpdate) at flush time.
    #[tokio::test]
    async fn test_touch_thread_records_live_cwd_not_resume_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        // Thread was created in /dir2.
        let tid = conv_mgr.create_thread(ThreadMeta {
            host: Some("h1".into()),
            cwd: Some("/dir2".into()),
            ..Default::default()
        });

        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        // Resume happens from /dir1.
        let mut attrs = HashMap::new();
        attrs.insert("hostname".to_string(), "h1".to_string());
        attrs.insert("shell_cwd".to_string(), "/dir1".to_string());
        mgr.register("s1", None, attrs, None).await.unwrap();

        // build_resumed_chat_ready stashes the resume-time cwd (/dir1) as pending_meta.
        let active: ActiveThreads = Arc::new(Mutex::new(HashMap::new()));
        active.lock().await.insert(
            tid.clone(),
            ThreadClaim {
                session_id: "s1".to_string(),
                last_active: std::time::Instant::now(),
                pending_meta: Some(PendingMeta {
                    host: Some("h1".into()),
                    cwd: Some("/dir1".into()),
                }),
            },
        );

        // User chose CdToOld -> client cd'd back to /dir2 and pushed a
        // SessionUpdate, so the session's live cwd is now /dir2 before the
        // first message.
        let mut upd = HashMap::new();
        upd.insert("shell_cwd".to_string(), "/dir2".to_string());
        mgr.update_attrs("s1", 1, upd).await.unwrap();

        touch_thread(&active, &tid, &conv_mgr, &mgr).await;

        // The thread's cwd must stay /dir2, not become the resume-time /dir1.
        assert_eq!(conv_mgr.load_meta(&tid).cwd.as_deref(), Some("/dir2"));
        // pending_meta consumed on flush.
        assert!(active.lock().await.get(&tid).unwrap().pending_meta.is_none());
    }

    /// Issue #679 wiring: touch_thread must queue a `<event>host changed ...`
    /// marker when the flushed host differs from the thread's stored host. This
    /// also covers the review's "probe lands late" case: the ChatStart snapshot
    /// host is None, but the live probe reports the new host at flush time.
    #[tokio::test]
    async fn test_touch_thread_queues_host_change_event() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let tid = conv_mgr.create_thread(ThreadMeta {
            host: Some("old".into()),
            cwd: Some("/p".into()),
            ..Default::default()
        });

        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        // ChatStart snapshot had no hostname (probe lagged), live probe now says "new".
        let mut attrs = HashMap::new();
        attrs.insert("hostname".to_string(), "new".to_string());
        attrs.insert("shell_cwd".to_string(), "/p".to_string());
        mgr.register("s1", None, attrs, None).await.unwrap();

        let active: ActiveThreads = Arc::new(Mutex::new(HashMap::new()));
        active.lock().await.insert(
            tid.clone(),
            ThreadClaim {
                session_id: "s1".to_string(),
                last_active: std::time::Instant::now(),
                pending_meta: Some(PendingMeta {
                    host: None,
                    cwd: Some("/p".into()),
                }),
            },
        );

        touch_thread(&active, &tid, &conv_mgr, &mgr).await;

        let meta = conv_mgr.load_meta(&tid);
        assert_eq!(meta.host.as_deref(), Some("new"));
        assert_eq!(
            meta.pending_event.as_deref(),
            Some("<event>host changed from old to new</event>")
        );
    }

    /// Issue #679: a same-host resume must not queue a host marker.
    #[tokio::test]
    async fn test_touch_thread_same_host_queues_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let tid = conv_mgr.create_thread(ThreadMeta {
            host: Some("h1".into()),
            cwd: Some("/p".into()),
            ..Default::default()
        });

        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let mut attrs = HashMap::new();
        attrs.insert("hostname".to_string(), "h1".to_string());
        attrs.insert("shell_cwd".to_string(), "/p".to_string());
        mgr.register("s1", None, attrs, None).await.unwrap();

        let active: ActiveThreads = Arc::new(Mutex::new(HashMap::new()));
        active.lock().await.insert(
            tid.clone(),
            ThreadClaim {
                session_id: "s1".to_string(),
                last_active: std::time::Instant::now(),
                pending_meta: Some(PendingMeta {
                    host: Some("h1".into()),
                    cwd: Some("/p".into()),
                }),
            },
        );

        touch_thread(&active, &tid, &conv_mgr, &mgr).await;
        assert_eq!(conv_mgr.load_meta(&tid).pending_event, None);
    }

    /// Issue #679 review: a blank hostname probe must not overwrite a real host
    /// with `Some("")` (which would permanently defeat change detection). The
    /// resume snapshot is used instead, and still yields the change marker.
    #[tokio::test]
    async fn test_touch_thread_blank_live_host_falls_back_to_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let tid = conv_mgr.create_thread(ThreadMeta {
            host: Some("old".into()),
            ..Default::default()
        });

        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let mut attrs = HashMap::new();
        attrs.insert("hostname".to_string(), String::new());
        mgr.register("s1", None, attrs, None).await.unwrap();

        let active: ActiveThreads = Arc::new(Mutex::new(HashMap::new()));
        active.lock().await.insert(
            tid.clone(),
            ThreadClaim {
                session_id: "s1".to_string(),
                last_active: std::time::Instant::now(),
                pending_meta: Some(PendingMeta {
                    host: Some("new".into()),
                    cwd: None,
                }),
            },
        );

        touch_thread(&active, &tid, &conv_mgr, &mgr).await;
        let meta = conv_mgr.load_meta(&tid);
        assert_eq!(meta.host.as_deref(), Some("new"));
        assert_eq!(
            meta.pending_event.as_deref(),
            Some("<event>host changed from old to new</event>")
        );
    }

    /// Issue #679 review: when neither the probe nor the snapshot has a usable
    /// host, keep the stored host rather than clearing it to `None`/`Some("")`.
    #[tokio::test]
    async fn test_touch_thread_no_usable_host_keeps_stored_host() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = ConversationManager::new(dir.path().join("threads"));
        let tid = conv_mgr.create_thread(ThreadMeta {
            host: Some("old".into()),
            ..Default::default()
        });

        let mgr = SessionManager::new(dir.path().to_path_buf(), Default::default());
        let mut attrs = HashMap::new();
        attrs.insert("hostname".to_string(), "  ".to_string());
        mgr.register("s1", None, attrs, None).await.unwrap();

        let active: ActiveThreads = Arc::new(Mutex::new(HashMap::new()));
        active.lock().await.insert(
            tid.clone(),
            ThreadClaim {
                session_id: "s1".to_string(),
                last_active: std::time::Instant::now(),
                pending_meta: Some(PendingMeta { host: None, cwd: None }),
            },
        );

        touch_thread(&active, &tid, &conv_mgr, &mgr).await;
        let meta = conv_mgr.load_meta(&tid);
        assert_eq!(meta.host.as_deref(), Some("old"));
        assert_eq!(meta.pending_event, None);
    }

    /// Issue #684: `/thread deleted` lists tombstoned threads with ids the
    /// restore command can resolve, and says so when there are none.
    #[tokio::test]
    async fn test_format_deleted_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = Arc::new(ConversationManager::new(dir.path().join("threads")));

        // Nothing deleted yet.
        let out = format_deleted_conversations_json(&conv_mgr, 20);
        assert!(out.get("thread_ids").is_none());
        assert!(out["display"].as_str().unwrap().contains("No deleted conversations"));

        let tid = conv_mgr.create_thread(ThreadMeta::default());
        conv_mgr.append_messages(
            &tid,
            &[serde_json::json!({"role": "user", "content": "how do I grep?"})],
        );
        assert!(conv_mgr.delete_thread(&tid));

        let out = format_deleted_conversations_json(&conv_mgr, 20);
        let ids: Vec<&str> = out["thread_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![tid.as_str()]);
        let display = out["display"].as_str().unwrap();
        assert!(display.contains(&tid[..8]), "short id shown: {display}");
        assert!(display.contains("how do I grep?"), "last question: {display}");
        assert!(display.contains("/thread restore"), "hint shown: {display}");
    }

    /// Issue #683: a network drop can flip macOS `gethostname()` between `X`
    /// and `X.local`. The `/resume` current-host filter must still match, or
    /// every conversation disappears ("no conversations to resume").
    #[tokio::test]
    async fn test_conversations_current_host_filter_matches_dotlocal() {
        let dir = tempfile::tempdir().unwrap();
        let conv_mgr = Arc::new(ConversationManager::new(dir.path().join("threads")));

        let mac = conv_mgr.create_thread(ThreadMeta {
            host: Some("MBProM4HF-2.local".into()),
            ..Default::default()
        });
        let other = conv_mgr.create_thread(ThreadMeta {
            host: Some("titan".into()),
            ..Default::default()
        });
        let active: ActiveThreads = Arc::new(Mutex::new(HashMap::new()));

        let ids = |v: &serde_json::Value| -> Vec<String> {
            v.get("thread_ids")
                .and_then(|t| t.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };

        // Session reports the bare name after the flip; the `.local` thread
        // must still be listed (this returned "No conversations yet" before).
        let out =
            format_conversations_json(&conv_mgr, &active, None, Some("MBProM4HF-2".into())).await;
        assert_eq!(ids(&out), vec![mac.clone()]);

        // And the other direction (thread bare, session `.local`) also matches.
        let out =
            format_conversations_json(&conv_mgr, &active, None, Some("titan.local".into())).await;
        assert_eq!(ids(&out), vec![other.clone()]);

        // A genuinely different host still filters everything out.
        let out =
            format_conversations_json(&conv_mgr, &active, None, Some("falcon".into())).await;
        assert!(ids(&out).is_empty(), "unrelated host must not match");
    }

    /// Issue #682: interrupt elapsed uses the earliest unfinished tool, falls
    /// back to the LLM call, then to the loop start.
    #[test]
    fn test_interrupt_start_resolution() {
        let loop_start = std::time::Instant::now();
        let llm_start = loop_start + std::time::Duration::from_millis(10);
        let earlier_tool = loop_start + std::time::Duration::from_millis(20);
        let later_tool = loop_start + std::time::Duration::from_millis(30);

        let mut started = HashMap::new();
        started.insert("a".to_string(), later_tool);
        started.insert("b".to_string(), earlier_tool);

        // Earliest unfinished tool wins regardless of iteration order.
        assert_eq!(
            interrupt_start(&started, ["a", "b"].into_iter(), Some(llm_start), loop_start),
            earlier_tool
        );
        // No started unfinished tool -> LLM call.
        assert_eq!(
            interrupt_start(&started, ["missing"].into_iter(), Some(llm_start), loop_start),
            llm_start
        );
        // Nothing known -> loop start.
        assert_eq!(
            interrupt_start(&started, ["missing"].into_iter(), None, loop_start),
            loop_start
        );
    }

    #[tokio::test]
    async fn test_reconstruct_history_renders_compact_summary() {
        let registry = omnish_daemon::tool_registry::ToolRegistry::new();
        let formatter = omnish_daemon::formatter_mgr::FormatterManager::new();
        let mut summary = omnish_daemon::compact::make_compact_message("THE SUMMARY", 1);
        summary["content"].as_array_mut().unwrap().push(serde_json::json!({"type": "text", "text": "next q"}));
        let msgs = vec![
            summary,
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "answer"}]}),
        ];
        let h = reconstruct_history(&msgs, &registry, &formatter).await;
        let pairs: Vec<(&str, &str)> = h
            .iter()
            .map(|e| (e["type"].as_str().unwrap(), e["text"].as_str().unwrap_or("")))
            .collect();
        assert_eq!(pairs[0], ("user_input", "/compact"));
        assert_eq!(pairs[1], ("response", "THE SUMMARY"));
        assert_eq!(pairs[2], ("separator", ""));
        assert_eq!(pairs[3], ("user_input", "next q"));

        // An auto compaction (issue #691) is labeled as such.
        let mut auto = omnish_daemon::compact::make_compact_message("S", 1);
        auto[omnish_daemon::compact::COMPACT_AUTO_FIELD] = serde_json::json!(true);
        let h = reconstruct_history(&[auto], &registry, &formatter).await;
        assert_eq!(h[0]["text"], "/compact (auto)");
    }

    #[test]
    fn test_usage_tokens() {
        assert_eq!(usage_tokens(10, 20, 30, 40), Some(100));
        assert_eq!(usage_tokens(0, 0, 0, 0), None);
    }

    fn compact_test_response(content: Vec<ContentBlock>) -> omnish_llm::backend::LlmResponse {
        omnish_llm::backend::LlmResponse {
            content,
            stop_reason: StopReason::EndTurn,
            model: "m".to_string(),
            usage: None,
        }
    }

    #[test]
    fn test_compact_turn_request_replays_chat_prefix() {
        use omnish_llm::backend::CacheHint;
        let window = vec![
            serde_json::json!({"role": "user", "content": "q1"}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "a1"}]}),
            serde_json::json!({"role": "user", "content": "q2"}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "a2"}]}),
        ];
        let tool = omnish_llm::tool::ToolDef {
            name: "bash".to_string(),
            description: "run".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            cache: CacheHint::None,
        };
        let req = build_compact_turn_request(&window, "SYS".to_string(), vec![tool], "INS", "sess", Some(9));

        // Same system prompt / tools / thinking mode as the chat turn.
        let sys = req.system_prompt.as_ref().unwrap();
        assert_eq!(sys.text, "SYS");
        assert_eq!(sys.cache, CacheHint::Long);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "bash");
        assert_eq!(req.enable_thinking, Some(true));
        assert_eq!(req.max_content_chars, Some(9));
        assert!(req.context.is_empty() && req.query.is_none());

        // History unchanged, instruction appended as the only new message.
        let msgs: Vec<&serde_json::Value> = req.extra_messages.iter().map(|m| &m.content).collect();
        assert_eq!(msgs.len(), 5);
        for (i, m) in window.iter().enumerate() {
            assert_eq!(msgs[i], m);
        }
        assert_eq!(msgs[4], &serde_json::json!({"role": "user", "content": "<system-compaction>\nINS\n</system-compaction>"}));

        // Chat breakpoint scheme: last two messages.
        let hints: Vec<CacheHint> = req.extra_messages.iter().map(|m| m.cache).collect();
        assert_eq!(hints, vec![CacheHint::None, CacheHint::None, CacheHint::None, CacheHint::Long, CacheHint::Long]);
    }

    #[test]
    fn test_compact_transcript_request_has_no_tools_or_system() {
        let window = vec![serde_json::json!({"role": "user", "content": "hello"})];
        let req = build_compact_transcript_request(&window, 1000, "en", None);
        assert!(req.tools.is_empty());
        assert!(req.system_prompt.is_none());
        assert!(req.extra_messages.is_empty());
        assert_eq!(req.enable_thinking, Some(false));
        assert!(req.context.contains("<conversation>\nUser: hello\n</conversation>"));
        assert!(req.query.unwrap().starts_with("Below is a <conversation>"));
    }

    #[test]
    fn test_compact_summary_from_response() {
        let ok = compact_test_response(vec![
            ContentBlock::Thinking { thinking: "hmm".to_string(), signature: None },
            ContentBlock::Text("  THE SUMMARY \n".to_string()),
        ]);
        assert_eq!(compact_summary_from_response(&ok).unwrap(), "THE SUMMARY");

        let tool = compact_test_response(vec![
            ContentBlock::Text("let me check".to_string()),
            ContentBlock::ToolUse(omnish_llm::tool::ToolCall {
                id: "t".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
                extra: Default::default(),
            }),
        ]);
        assert!(compact_summary_from_response(&tool).unwrap_err().contains("tool"));

        let empty = compact_test_response(vec![ContentBlock::Text("   ".to_string())]);
        assert!(compact_summary_from_response(&empty).is_err());
    }

    #[test]
    fn test_format_full_compact() {
        let o = FullCompactOutcome {
            chars_before: 10_000,
            chars_after: 1_000,
            usage: Some(omnish_llm::backend::Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 900,
                cache_creation_input_tokens: 0,
            }),
            fallback: None,
        };
        let text = format_full_compact(&o);
        assert!(text.contains("(-90%)"));
        assert!(text.contains("LLM input: 1.0K tokens (900 cached)"), "{}", text);
        assert!(!text.contains("transcript"));

        let fb = FullCompactOutcome { usage: None, fallback: Some("boom".to_string()), ..o };
        let text = format_full_compact(&fb);
        assert!(!text.contains("LLM input"));
        assert!(text.contains("(summarized from transcript: boom)"));
    }

    #[test]
    fn test_compose_chat_system_prompt() {
        assert_eq!(compose_chat_system_prompt("B", "R", None), "B\n\nR");
        assert_eq!(compose_chat_system_prompt("B", "R", Some("P")), "B\n\nR\n\nP");
    }

    fn image_registry() -> omnish_daemon::tool_registry::ToolRegistry {
        let reg = omnish_daemon::tool_registry::ToolRegistry::new();
        for (name, images) in [("read_image", true), ("read", false)] {
            reg.register(omnish_daemon::tool_registry::ToolMeta {
                name: name.to_string(),
                display_name: name.to_string(),
                formatter: "default".to_string(),
                status_template: String::new(),
                custom_status: None,
                plugin_type: None,
                plugin_name: None,
                summarization_prompt: None,
                result_images: images,
            });
        }
        reg
    }

    fn tool_result(content: &str, is_error: bool) -> omnish_llm::tool::ToolResult {
        omnish_llm::tool::ToolResult { tool_use_id: "t1".into(), content: content.into(), is_error }
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn test_tool_result_json_image() {
        let reg = image_registry();
        let content = omnish_common::tool_image::encode("image/png", PNG, "Image: /tmp/a.png (1x1, png, 1 KB)");
        let v = tool_result_json(&tool_result(&content, false), Some("read_image"), &reg);
        assert_eq!(v["type"], "tool_result");
        assert_eq!(v["tool_use_id"], "t1");
        assert_eq!(v["is_error"], false);
        let blocks = v["content"].as_array().expect("array content");
        assert_eq!(blocks[0], serde_json::json!({"type": "text", "text": "Image: /tmp/a.png (1x1, png, 1 KB)"}));
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert!(blocks[1]["source"]["data"].as_str().unwrap().len() > 4);
        assert_eq!(image_description(&content, Some("read_image"), &reg).as_deref(), Some("Image: /tmp/a.png (1x1, png, 1 KB)"));
    }

    #[test]
    fn test_tool_result_json_stays_text_otherwise() {
        let reg = image_registry();
        let content = omnish_common::tool_image::encode("image/png", PNG, "d");
        // Tool without the flag: never parsed.
        assert_eq!(tool_result_json(&tool_result(&content, false), Some("read"), &reg)["content"], content.as_str());
        // Unknown tool.
        assert_eq!(tool_result_json(&tool_result(&content, false), None, &reg)["content"], content.as_str());
        // Errors stay text.
        assert_eq!(tool_result_json(&tool_result(&content, true), Some("read_image"), &reg)["content"], content.as_str());
        // Not an image block.
        assert_eq!(tool_result_json(&tool_result("Error: nope", false), Some("read_image"), &reg)["content"], "Error: nope");
        assert!(image_description("plain", Some("read_image"), &reg).is_none());
    }

    #[tokio::test]
    async fn test_reconstruct_history_shows_image_description() {
        let reg = image_registry();
        let formatter = omnish_daemon::formatter_mgr::FormatterManager::new();
        let content = omnish_common::tool_image::encode("image/png", PNG, "Image: /tmp/a.png (1x1, png, 1 KB)");
        let r = tool_result(&content, false);
        let msgs = vec![
            serde_json::json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "read_image", "input": {"file_path": "/tmp/a.png"}}
            ]}),
            serde_json::json!({"role": "user", "content": [tool_result_json(&r, Some("read_image"), &reg)]}),
        ];
        let h = reconstruct_history(&msgs, &reg, &formatter).await;
        let status = h.iter().find(|e| e["type"] == "tool_status").expect("tool status entry");
        let full = serde_json::to_string(&status["result_full"]).unwrap();
        assert!(full.contains("Image: /tmp/a.png (1x1, png, 1 KB)"), "{}", full);
        assert!(!full.contains("base64") && !full.contains("iVBOR"));
    }
}
