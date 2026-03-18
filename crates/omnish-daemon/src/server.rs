use anyhow::Result;
use omnish_daemon::conversation_mgr::{ConversationManager, ThreadMeta};
use omnish_daemon::formatter::{self, FormatInput};
use omnish_daemon::plugin::{PluginManager, PluginType};
use omnish_daemon::session_mgr::SessionManager;
use omnish_daemon::task_mgr::TaskManager;
use omnish_llm::backend::{ContentBlock, LlmBackend, LlmRequest, StopReason, TriggerType, UseCase};
use omnish_protocol::message::*;
use omnish_transport::rpc_server::RpcServer;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
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
    messages: Vec<Message>,
    iteration: usize,
    cm: ChatMessage,
    start: std::time::Instant,
    command_query_tool: omnish_daemon::tools::command_query::CommandQueryTool,
}

pub struct DaemonServer {
    session_mgr: Arc<SessionManager>,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    task_mgr: Arc<Mutex<TaskManager>>,
    conv_mgr: Arc<ConversationManager>,
    plugin_mgr: Arc<PluginManager>,
    pending_agent_loops: Arc<Mutex<HashMap<String, AgentLoopState>>>,
    chat_model_name: Option<String>,
}

impl DaemonServer {
    pub fn new(
        session_mgr: Arc<SessionManager>,
        llm_backend: Option<Arc<dyn LlmBackend>>,
        task_mgr: Arc<Mutex<TaskManager>>,
        conv_mgr: Arc<ConversationManager>,
        plugin_mgr: Arc<PluginManager>,
        chat_model_name: Option<String>,
    ) -> Self {
        Self {
            session_mgr,
            llm_backend,
            task_mgr,
            conv_mgr,
            plugin_mgr,
            pending_agent_loops: Arc::new(Mutex::new(HashMap::new())),
            chat_model_name,
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
        let pending_loops = self.pending_agent_loops.clone();
        let chat_model_name = self.chat_model_name.clone();

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

        server
            .serve(
                move |msg| {
                    let mgr = mgr.clone();
                    let llm = llm.clone();
                    let task_mgr = task_mgr.clone();
                    let conv_mgr = conv_mgr.clone();
                    let plugin_mgr = plugin_mgr.clone();
                    let pending_loops = pending_loops.clone();
                    let chat_model_name = chat_model_name.clone();
                    Box::pin(async move { handle_message(msg, mgr, &llm, &task_mgr, &conv_mgr, &plugin_mgr, &pending_loops, &chat_model_name).await })
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
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
    chat_model_name: &Option<String>,
) -> Vec<Message> {
    // Shadow with reference for existing code; use mgr_arc for spawned tasks
    let mgr_arc = mgr;
    let mgr = &*mgr_arc;
    vec![match msg {
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
                let result = handle_builtin_command(&req, mgr, task_mgr, llm, conv_mgr, plugin_mgr).await;
                let content = serde_json::to_string(&result).unwrap_or_else(|_| {
                    r#"{"display":"(serialization error)"}"#.to_string()
                });
                return vec![Message::Response(Response {
                    request_id: req.request_id,
                    content,
                    is_streaming: false,
                    is_final: true,
                })];
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
            // Debug shortcut: return canned suggestions for testing
            if req.input.trim() == "omnish_debug" {
                return vec![Message::CompletionResponse(omnish_protocol::message::CompletionResponse {
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
                })];
            }
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
            let meta = {
                let host = mgr.get_session_attr(&cs.session_id, "hostname").await;
                let cwd = mgr.get_session_attr(&cs.session_id, "shell_cwd").await;
                ThreadMeta { host, cwd, ..Default::default() }
            };
            let thread_id = if cs.new_thread {
                conv_mgr.create_thread(meta)
            } else {
                let tid = conv_mgr.get_latest_thread()
                    .unwrap_or_else(|| conv_mgr.create_thread(meta.clone()));
                // Update meta for existing thread
                conv_mgr.save_meta(&tid, &meta);
                tid
            };
            Message::ChatReady(ChatReady {
                request_id: cs.request_id,
                thread_id,
                last_exchange: None,
                earlier_count: 0,
                model_name: chat_model_name.clone(),
            })
        }
        Message::ChatMessage(cm) => {
            return handle_chat_message(cm, mgr, llm, conv_mgr, plugin_mgr, pending_loops).await;
        }
        Message::ChatToolResult(tr) => {
            return handle_tool_result(tr, mgr, llm, conv_mgr, plugin_mgr, pending_loops).await;
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
            } else {
                // No pending loop — just record the interrupt
                conv_mgr.append_messages(&ci.thread_id, &[
                    serde_json::json!({"role": "user", "content": ci.query}),
                    serde_json::json!({"role": "assistant", "content": "<event>user interrupted</event>"}),
                ]);
            }

            tracing::info!("Chat interrupted by user (thread={}, request={})", ci.thread_id, ci.request_id);
            Message::Ack
        }
        _ => Message::Ack,
    }]
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

async fn build_chat_setup(mgr: &SessionManager, plugin_mgr: &PluginManager) -> ChatSetup {
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(
        commands,
        stream_reader,
    );

    let mut tools = command_query_tool.definitions();
    tools.extend(plugin_mgr.all_tools());

    // Load base chat prompt, then apply user overrides from chat.override.json
    let pm = load_chat_prompt();
    let system_prompt = pm.build();

    ChatSetup { command_query_tool, tools, system_prompt }
}

async fn handle_chat_message(
    cm: ChatMessage,
    mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
) -> Vec<Message> {
    if llm.is_none() {
        return vec![Message::ChatResponse(ChatResponse {
            request_id: cm.request_id,
            thread_id: cm.thread_id,
            content: "(LLM backend not configured)".to_string(),
        })];
    }
    let backend = llm.as_ref().unwrap();

    let use_case = UseCase::Chat;
    let max_context_chars = backend.max_content_chars_for_use_case(use_case);

    let ChatSetup { command_query_tool, tools, system_prompt } =
        build_chat_setup(mgr, plugin_mgr).await;

    // Get live cwd from session probe (if available)
    let live_cwd = mgr.get_session_attr(&cm.session_id, "shell_cwd").await;

    // Build system-reminder with time, cwd, and last 5 commands
    let reminder = command_query_tool.build_system_reminder(5, live_cwd.as_deref());

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
        messages: vec![],
        iteration: 0,
        cm,
        start: std::time::Instant::now(),
        command_query_tool,
    };

    run_agent_loop(state, llm, conv_mgr, plugin_mgr, pending_loops).await
}

/// Handle a ChatToolResult from the client — accumulate results, resume when all are received.
async fn handle_tool_result(
    tr: ChatToolResult,
    _mgr: &SessionManager,
    llm: &Option<Arc<dyn LlmBackend>>,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
) -> Vec<Message> {
    let mut map = pending_loops.lock().await;
    let state = match map.get_mut(&tr.request_id) {
        Some(s) => s,
        None => {
            tracing::warn!("No pending agent loop for request_id={}", tr.request_id);
            return vec![Message::Ack];
        }
    };

    // Check if the agent loop has timed out (10 minutes — bash commands like builds can be slow)
    if state.start.elapsed() > std::time::Duration::from_secs(600) {
        map.remove(&tr.request_id);
        drop(map);
        tracing::warn!("Agent loop timed out for request_id={}", tr.request_id);
        return vec![Message::ChatResponse(ChatResponse {
            request_id: tr.request_id,
            thread_id: tr.thread_id,
            content: "Error: client-side tool execution timed out".to_string(),
        })];
    }

    // Add the received client-side tool result
    state.completed_results.push(omnish_llm::tool::ToolResult {
        tool_use_id: tr.tool_call_id.clone(),
        content: tr.content,
        is_error: tr.is_error,
    });

    // Check if all tool calls are now completed
    let completed_ids: std::collections::HashSet<String> = state
        .completed_results
        .iter()
        .map(|r| r.tool_use_id.clone())
        .collect();
    let all_complete = state.pending_tool_calls.iter().all(|tc| completed_ids.contains(&tc.id));

    if !all_complete {
        // More results expected from client — acknowledge and wait
        drop(map);
        return vec![Message::Ack];
    }

    // All tool calls complete — remove state and continue agent loop
    let mut state = map.remove(&tr.request_id).unwrap();
    drop(map);

    // Generate post-execution ChatToolStatus for each completed client-side tool
    let mut update_messages = Vec::new();
    for result in &state.completed_results {
        // Find the original tool call to get name/input for formatting
        if let Some(tc) = state.pending_tool_calls.iter().find(|tc| tc.id == result.tool_use_id) {
            let fmt = formatter::get_formatter(
                plugin_mgr.tool_formatter(&tc.name).unwrap_or("default")
            );
            let display_name = plugin_mgr.tool_display_name(&tc.name)
                .unwrap_or(&tc.name).to_string();
            let status_template = plugin_mgr.tool_status_template(&tc.name)
                .unwrap_or("").to_string();
            let fmt_out = fmt.format(&FormatInput {
                tool_name: tc.name.clone(),
                display_name: display_name.clone(),
                status_template,
                params: tc.input.clone(),
                output: Some(result.content.clone()),
                is_error: Some(result.is_error),
            });
            update_messages.push(Message::ChatToolStatus(ChatToolStatus {
                request_id: state.cm.request_id.clone(),
                thread_id: state.cm.thread_id.clone(),
                tool_name: tc.name.clone(),
                tool_call_id: Some(tc.id.clone()),
                status: String::new(),
                status_icon: Some(fmt_out.status_icon),
                display_name: Some(display_name),
                param_desc: Some(fmt_out.param_desc),
                result_compact: Some(fmt_out.result_compact),
                result_full: Some(fmt_out.result_full),
            }));
        }
    }

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

    // Prepend update messages before agent loop continuation messages
    let mut loop_messages = run_agent_loop(state, llm, conv_mgr, plugin_mgr, pending_loops).await;
    update_messages.append(&mut loop_messages);
    update_messages
}

/// Core agent loop: calls LLM, executes tools, pauses on client-side tools.
/// Used by both `handle_chat_message` (initial) and `handle_tool_result` (resumption).
async fn run_agent_loop(
    mut state: AgentLoopState,
    llm: &Option<Arc<dyn LlmBackend>>,
    conv_mgr: &Arc<ConversationManager>,
    plugin_mgr: &Arc<PluginManager>,
    pending_loops: &Arc<Mutex<HashMap<String, AgentLoopState>>>,
) -> Vec<Message> {
    let backend = match llm {
        Some(b) => b,
        None => {
            return vec![Message::ChatResponse(ChatResponse {
                request_id: state.cm.request_id,
                thread_id: state.cm.thread_id,
                content: "(LLM backend not configured)".to_string(),
            })];
        }
    };

    let max_iterations = 30;
    let mut messages = std::mem::take(&mut state.messages);

    for iteration in state.iteration..max_iterations {
        match backend.complete(&state.llm_req).await {
            Ok(response) => {
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

                    // Send LLM's text blocks to client (e.g., "I'll run this command")
                    for block in &response.content {
                        if let ContentBlock::Text(text) = block {
                            if !text.trim().is_empty() {
                                messages.push(Message::ChatToolStatus(ChatToolStatus {
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
                                }));
                            }
                        }
                    }

                    // Execute daemon-side tools immediately, forward all client-side tools
                    let mut tool_results = Vec::new();
                    let mut has_client_tools = false;
                    for tc in &tool_calls {
                        let display_name = plugin_mgr.tool_display_name(&tc.name)
                            .unwrap_or(&tc.name).to_string();
                        let formatter_name = plugin_mgr.tool_formatter(&tc.name)
                            .unwrap_or("default");
                        let status_template = plugin_mgr.tool_status_template(&tc.name)
                            .unwrap_or("").to_string();
                        let fmt = formatter::get_formatter(formatter_name);
                        let fmt_out = fmt.format(&FormatInput {
                            tool_name: tc.name.clone(),
                            display_name: display_name.clone(),
                            status_template,
                            params: tc.input.clone(),
                            output: None,
                            is_error: None,
                        });

                        messages.push(Message::ChatToolStatus(ChatToolStatus {
                            request_id: state.cm.request_id.clone(),
                            thread_id: state.cm.thread_id.clone(),
                            tool_name: tc.name.clone(),
                            tool_call_id: Some(tc.id.clone()),
                            status: String::new(),
                            status_icon: Some(fmt_out.status_icon),
                            display_name: Some(display_name),
                            param_desc: Some(fmt_out.param_desc),
                            result_compact: None,
                            result_full: None,
                        }));

                        let ptype = plugin_mgr.tool_plugin_type(&tc.name);
                        if ptype == Some(PluginType::ClientTool) {
                            // Client-side tool: forward to client for parallel execution
                            messages.push(Message::ChatToolCall(ChatToolCall {
                                request_id: state.cm.request_id.clone(),
                                thread_id: state.cm.thread_id.clone(),
                                tool_name: tc.name.clone(),
                                tool_call_id: tc.id.clone(),
                                input: serde_json::to_string(&tc.input).unwrap_or_default(),
                                plugin_name: plugin_mgr.tool_plugin_name(&tc.name).unwrap_or("builtin").to_string(),
                                sandboxed: plugin_mgr.tool_sandboxed(&tc.name).unwrap_or(true),
                            }));
                            has_client_tools = true;
                        } else {
                            // Daemon-side tool: execute directly
                            let mut result = if tc.name == "omnish_list_history" || tc.name == "omnish_get_output" {
                                state.command_query_tool.execute(&tc.name, &tc.input)
                            } else {
                                omnish_llm::tool::ToolResult {
                                    tool_use_id: String::new(),
                                    content: format!("Unknown daemon tool: {}", tc.name),
                                    is_error: true,
                                }
                            };
                            result.tool_use_id = tc.id.clone();

                            // Post-execution: send update ChatToolStatus with formatted results
                            let post_fmt = formatter::get_formatter(
                                plugin_mgr.tool_formatter(&tc.name).unwrap_or("default")
                            );
                            let post_display = plugin_mgr.tool_display_name(&tc.name)
                                .unwrap_or(&tc.name).to_string();
                            let post_template = plugin_mgr.tool_status_template(&tc.name)
                                .unwrap_or("").to_string();
                            let post_out = post_fmt.format(&FormatInput {
                                tool_name: tc.name.clone(),
                                display_name: post_display.clone(),
                                status_template: post_template,
                                params: tc.input.clone(),
                                output: Some(result.content.clone()),
                                is_error: Some(result.is_error),
                            });
                            messages.push(Message::ChatToolStatus(ChatToolStatus {
                                request_id: state.cm.request_id.clone(),
                                thread_id: state.cm.thread_id.clone(),
                                tool_name: tc.name.clone(),
                                tool_call_id: Some(tc.id.clone()),
                                status: String::new(),
                                status_icon: Some(post_out.status_icon),
                                display_name: Some(post_display),
                                param_desc: Some(post_out.param_desc),
                                result_compact: Some(post_out.result_compact),
                                result_full: Some(post_out.result_full),
                            }));

                            tool_results.push(result);
                        }
                    }

                    if has_client_tools {
                        // Pause loop — client will execute tools in parallel and send results back
                        state.pending_tool_calls = tool_calls.iter().map(|tc| (*tc).clone()).collect();
                        state.completed_results = tool_results;
                        state.messages = vec![];
                        state.iteration = iteration;
                        let request_id = state.cm.request_id.clone();
                        pending_loops.lock().await.insert(request_id, state);
                        return messages;
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
                messages.push(Message::ChatResponse(ChatResponse {
                    request_id: state.cm.request_id.clone(),
                    thread_id: state.cm.thread_id.clone(),
                    content: text,
                }));
                return messages;
            }
            Err(e) => {
                tracing::error!("Chat LLM failed: {}", e);
                messages.push(Message::ChatResponse(ChatResponse {
                    request_id: state.cm.request_id.clone(),
                    thread_id: state.cm.thread_id.clone(),
                    content: format!("Error: {}", e),
                }));
                return messages;
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
    messages.push(Message::ChatResponse(ChatResponse {
        request_id: state.cm.request_id,
        thread_id: state.cm.thread_id,
        content: text,
    }));
    messages
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
fn reconstruct_history(
    raw_messages: &[serde_json::Value],
    plugin_mgr: &PluginManager,
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
                                let formatter_name = plugin_mgr.tool_formatter(&tool_name)
                                    .unwrap_or("default");
                                let fmt = formatter::get_formatter(formatter_name);
                                let display_name = plugin_mgr.tool_display_name(&tool_name)
                                    .unwrap_or(&tool_name).to_string();
                                let status_template = plugin_mgr.tool_status_template(&tool_name)
                                    .unwrap_or("").to_string();
                                let fmt_out = fmt.format(&FormatInput {
                                    tool_name: tool_name.clone(),
                                    display_name: display_name.clone(),
                                    status_template,
                                    params: input,
                                    output: Some(output),
                                    is_error: Some(is_error),
                                });
                                let icon_str = match fmt_out.status_icon {
                                    omnish_protocol::message::StatusIcon::Running => "running",
                                    omnish_protocol::message::StatusIcon::Success => "success",
                                    omnish_protocol::message::StatusIcon::Error => "error",
                                };
                                entries.push(serde_json::json!({
                                    "type": "tool_status",
                                    "tool_name": tool_name,
                                    "tool_call_id": tool_use_id,
                                    "status_icon": icon_str,
                                    "display_name": display_name,
                                    "param_desc": fmt_out.param_desc,
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

async fn handle_builtin_command(req: &Request, mgr: &SessionManager, task_mgr: &Mutex<TaskManager>, llm_backend: &Option<Arc<dyn LlmBackend>>, conv_mgr: &Arc<ConversationManager>, plugin_mgr: &PluginManager) -> serde_json::Value {
    let sub = req.query.strip_prefix("__cmd:").unwrap_or("");

    // Build system-reminder for context display
    let (commands, stream_reader) = mgr.get_all_commands_with_reader().await;
    let command_query_tool = omnish_daemon::tools::command_query::CommandQueryTool::new(commands, stream_reader);
    let reminder = command_query_tool.build_system_reminder(5, None);

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
            Some(thread_id) => {
                let raw_msgs = conv_mgr.load_raw_messages(&thread_id);
                let history = reconstruct_history(&raw_msgs, plugin_mgr);
                serde_json::json!({
                    "thread_id": thread_id,
                    "history": history,
                })
            }
            None => cmd_display("Invalid index: out of bounds"),
        };
    }
    // Handle /resume_tid <thread_id> for resuming by thread ID (stable across deletions)
    if let Some(tid) = sub.strip_prefix("resume_tid ") {
        let tid = tid.trim();
        let raw_msgs = conv_mgr.load_raw_messages(tid);
        if raw_msgs.is_empty() {
            return cmd_display("Conversation not found");
        }
        let history = reconstruct_history(&raw_msgs, plugin_mgr);
        return serde_json::json!({
            "thread_id": tid,
            "history": history,
        });
    }
    // Handle /resume without index (resume latest = /resume 1)
    if sub == "resume" {
        return match conv_mgr.get_thread_by_index(0) {
            Some(thread_id) => {
                let raw_msgs = conv_mgr.load_raw_messages(&thread_id);
                let history = reconstruct_history(&raw_msgs, plugin_mgr);
                serde_json::json!({
                    "thread_id": thread_id,
                    "history": history,
                })
            }
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
    if let Some(name) = sub.strip_prefix("template ") {
        return cmd_display(handle_template(name, mgr, plugin_mgr).await);
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
        "conversations" => format_conversations_json(conv_mgr),
        "session" => match get_session_debug_info(&req.session_id, mgr).await {
            Ok(info) => cmd_display(info),
            Err(e) => cmd_display(format!("Error: {}", e)),
        },
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

async fn handle_template(name: &str, mgr: &SessionManager, plugin_mgr: &PluginManager) -> String {
    match name {
        "chat" => {
            let ChatSetup { tools, system_prompt, .. } =
                build_chat_setup(mgr, plugin_mgr).await;

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

/// Format the list of conversations as JSON with display string and thread_ids.
fn format_conversations_json(conv_mgr: &Arc<ConversationManager>) -> serde_json::Value {
    let conversations = conv_mgr.list_conversations();
    if conversations.is_empty() {
        return cmd_display("No conversations yet. Start a chat with :");
    }

    let mut output = String::from("Conversations:\n");
    let mut thread_ids = Vec::new();
    for (i, (thread_id, modified, exchange_count, last_question)) in conversations.into_iter().enumerate() {
        let time_ago = format_relative_time(modified);

        let meta = conv_mgr.load_meta(&thread_id);

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
    }
    serde_json::json!({
        "display": output,
        "thread_ids": thread_ids,
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
