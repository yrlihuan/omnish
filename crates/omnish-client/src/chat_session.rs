use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

use omnish_protocol::message::*;
use omnish_protocol::message::{ChatToolStatus, StatusIcon};
use omnish_pty::proxy::PtyProxy;
use omnish_transport::rpc_client::RpcClient;

use crate::{client_plugin, command, display, ghost_complete, markdown, widgets};
use widgets::scroll_view::ScrollView;

#[derive(Debug, Clone)]
pub enum ScrollEntry {
    UserInput(String),
    ToolStatus(ChatToolStatus),
    LlmText(String),
    Response(String),
    Separator,
    SystemMessage(String),
}

pub struct ChatSession {
    current_thread_id: Option<String>,
    cached_thread_ids: Vec<String>,
    chat_history: VecDeque<String>,
    history_index: Option<usize>,
    completer: ghost_complete::GhostCompleter,
    scroll_history: Vec<ScrollEntry>,
    thinking_visible: bool,
    has_activity: bool,
    pending_input: Option<String>,
    client_plugins: Arc<client_plugin::ClientPluginManager>,
}

fn write_stdout(s: &str) {
    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
}

impl ChatSession {
    pub fn new(chat_history: VecDeque<String>) -> Self {
        Self {
            current_thread_id: None,
            cached_thread_ids: Vec::new(),
            chat_history,
            history_index: None,
            completer: ghost_complete::GhostCompleter::new(vec![
                Box::new(ghost_complete::BuiltinProvider::new()),
            ]),
            scroll_history: Vec::new(),
            thinking_visible: false,
            has_activity: false,
            pending_input: None,
            client_plugins: Arc::new(client_plugin::ClientPluginManager::new()),
        }
    }

    pub fn into_history(self) -> VecDeque<String> {
        self.chat_history
    }

    fn show_thinking(&mut self) {
        write_stdout("\x1b[2m(thinking...)\x1b[0m\r\n");
        self.thinking_visible = true;
    }

    fn erase_thinking(&mut self) {
        if self.thinking_visible {
            write_stdout("\x1b[1A\r\x1b[K");
            self.thinking_visible = false;
        }
    }

    fn print_line(&mut self, line: &str) {
        write_stdout(line);
        write_stdout("\r\n");
    }

    fn push_entry(&mut self, entry: ScrollEntry) {
        self.scroll_history.push(entry);
    }

    fn browse_history(&self) {
        if self.scroll_history.is_empty() {
            return;
        }
        let (rows, cols) = super::get_terminal_size().unwrap_or((24, 80));
        let lines: Vec<String> = self.scroll_history.iter().flat_map(|entry| {
            match entry {
                ScrollEntry::UserInput(text) => {
                    text.lines().enumerate().map(|(i, line)| {
                        if i == 0 {
                            format!("\x1b[36m> \x1b[0m{}", line)
                        } else {
                            format!("  {}", line)
                        }
                    }).collect::<Vec<_>>()
                }
                ScrollEntry::ToolStatus(cts) => {
                    let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                    let param_desc = cts.param_desc.as_deref().unwrap_or("");
                    let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                    let mut lines = vec![display::render_tool_header_full(icon, display_name, param_desc)];
                    if let Some(ref full) = cts.result_full {
                        lines.extend(display::render_tool_output(full));
                    }
                    lines
                }
                ScrollEntry::LlmText(text) => vec![text.clone()],
                ScrollEntry::Response(content) => {
                    let rendered = super::markdown::render(content);
                    let rendered = format!("\x1b[97m●\x1b[0m {}", rendered);
                    rendered.split("\r\n").map(String::from).collect()
                }
                ScrollEntry::Separator => {
                    vec![display::render_separator(cols)]
                }
                ScrollEntry::SystemMessage(msg) => {
                    vec![format!("\x1b[2;37m{}\x1b[0m", msg)]
                }
            }
        }).collect();

        if lines.is_empty() {
            return;
        }

        let compact_h = (rows as usize / 3).max(3);
        let expanded_h = (rows as usize).saturating_sub(3);
        let mut sv = ScrollView::new(compact_h, expanded_h, cols as usize);
        for line in &lines {
            sv.push_line(line);
        }
        sv.run_browse();
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &mut self,
        rpc: &RpcClient,
        session_id: &str,
        proxy: &PtyProxy,
        initial_msg: Option<String>,
        client_debug_fn: &dyn Fn() -> String,
        auto_update_enabled: &AtomicBool,
        cursor_col: u16,
        cursor_row: u16,
    ) {
        self.pending_input = initial_msg;

        // Move past shell prompt to a new line
        write_stdout("\r\n");

        loop {
            let input = if let Some(msg) = self.pending_input.take() {
                msg
            } else {
                write_stdout("\x1b[36m> \x1b[0m");
                match self.read_input(!self.has_activity) {
                    Some(line) => {
                        write_stdout("\r\n");
                        line
                    }
                    None => break,
                }
            };

            let trimmed = input.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Add user input to scroll history for browse mode (Ctrl+O)
            self.push_entry(ScrollEntry::UserInput(trimmed.to_string()));

            let is_inspection = trimmed.starts_with("/debug")
                || trimmed.starts_with("/context")
                || trimmed.starts_with("/template");
            let auto_exit = !self.has_activity && is_inspection;

            self.has_activity = true;
            save_to_history(&mut self.chat_history, trimmed, 100);
            self.history_index = None;

            // /thread del [N]
            if trimmed == "/thread del" || trimmed.starts_with("/thread del ") {
                self.handle_thread_del(trimmed, session_id, rpc).await;
                continue;
            }

            // /thread list
            if trimmed == "/thread list" {
                self.handle_thread_list(session_id, rpc).await;
                continue;
            }

            // /resume [N]
            if trimmed == "/resume" || trimmed.starts_with("/resume ") {
                self.handle_resume(trimmed, session_id, rpc).await;
                continue;
            }

            // /context
            if trimmed == "/context" || trimmed.starts_with("/context ") {
                let (without_redirect, redirect) = command::parse_redirect_pub(trimmed);
                let (base_cmd, limit) = command::parse_limit_pub(without_redirect);
                let query = if base_cmd == "/context chat" {
                    if let Some(ref tid) = self.current_thread_id {
                        format!("__cmd:context chat:{}", tid)
                    } else {
                        "__cmd:context chat".to_string()
                    }
                } else if let Some(ref tid) = self.current_thread_id {
                    format!("__cmd:context chat:{}", tid)
                } else {
                    "__cmd:context".to_string()
                };
                let request_id = Uuid::new_v4().to_string()[..8].to_string();
                let request = Message::Request(Request {
                    request_id: request_id.clone(),
                    session_id: session_id.to_string(),
                    query,
                    scope: RequestScope::AllSessions,
                });
                match rpc.call(request).await {
                    Ok(Message::Response(resp)) if resp.request_id == request_id => {
                        let display_text = if let Some(json) = super::parse_cmd_response(&resp.content) {
                            super::cmd_display_str(&json)
                        } else {
                            resp.content
                        };
                        let display_text = if let Some(ref l) = limit {
                            command::apply_limit(&display_text, l)
                        } else {
                            display_text
                        };
                        if let Some(path) = redirect {
                            super::handle_command_result(&display_text, Some(path), proxy.child_pid() as u32);
                        } else {
                            let output = display::render_response(&display_text);
                            write_stdout(&output);
                        }
                    }
                    _ => {
                        write_stdout(&display::render_error("Failed to get context"));
                    }
                }
                if auto_exit { break; }
                continue;
            }

            // /update auto
            let (without_redirect, redirect) = command::parse_redirect_pub(trimmed);
            let (base_cmd, limit) = command::parse_limit_pub(without_redirect);
            if base_cmd == "/update auto" {
                let prev = auto_update_enabled.load(Ordering::Relaxed);
                auto_update_enabled.store(!prev, Ordering::Relaxed);
                let status = if !prev { "enabled" } else { "disabled" };
                let result = format!("Auto-update {}", status);
                let display_result = if let Some(ref l) = limit {
                    command::apply_limit(&result, l)
                } else {
                    result
                };
                if let Some(path) = redirect {
                    super::handle_command_result(&display_result, Some(path), proxy.child_pid() as u32);
                } else {
                    write_stdout(&display::render_response(&display_result));
                }
                if auto_exit { break; }
                continue;
            }

            // Other /commands
            if trimmed.starts_with('/')
                && super::handle_slash_command(
                    trimmed, session_id, rpc, proxy, client_debug_fn, cursor_col, cursor_row,
                )
                .await
            {
                if auto_exit { break; }
                continue;
            }

            // Lazily create thread
            if self.current_thread_id.is_none() {
                let req_id = Uuid::new_v4().to_string()[..8].to_string();
                let start_msg = Message::ChatStart(ChatStart {
                    request_id: req_id.clone(),
                    session_id: session_id.to_string(),
                    new_thread: true,
                });
                match rpc.call(start_msg).await {
                    Ok(Message::ChatReady(ready)) if ready.request_id == req_id => {
                        self.current_thread_id = Some(ready.thread_id);
                    }
                    _ => {
                        write_stdout(&display::render_error("Failed to start chat session"));
                        continue;
                    }
                }
            }

            // Show thinking indicator
            self.show_thinking();

            // Send ChatMessage
            let req_id = Uuid::new_v4().to_string()[..8].to_string();
            let chat_msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
                request_id: req_id.clone(),
                session_id: session_id.to_string(),
                thread_id: self.current_thread_id.clone().unwrap(),
                query: trimmed.to_string(),
            });

            // Ctrl-C cancellation
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let (stop_tx, stop_rx) = std::sync::mpsc::channel();
            tokio::task::spawn_blocking(move || {
                if wait_for_ctrl_c(stop_rx) {
                    let _ = cancel_tx.send(true);
                }
            });

            let rpc_result = rpc.call_stream(chat_msg);
            let mut interrupted = false;

            async fn wait_cancel(rx: &mut tokio::sync::watch::Receiver<bool>) {
                loop {
                    if *rx.borrow() { return; }
                    if rx.changed().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }

            // Race initial RPC call against Ctrl-C
            let stream_result;
            {
                let mut crx = cancel_rx.clone();
                tokio::select! {
                    result = rpc_result => { stream_result = Some(result); }
                    _ = wait_cancel(&mut crx) => {
                        interrupted = true;
                        stream_result = None;
                    }
                }
            }

            if let Some(result) = stream_result {
                match result {
                    Ok(mut rx) => {
                        'stream: loop {
                            // Phase 1: Collect messages
                            let mut tool_calls: Vec<ChatToolCall> = Vec::new();
                            let mut got_response = false;
                            loop {
                                let mut crx = cancel_rx.clone();
                                tokio::select! {
                                    msg = rx.recv() => {
                                        match msg {
                                            Some(Message::ChatToolStatus(cts)) => {
                                                self.erase_thinking();
                                                if cts.tool_name.is_empty() {
                                                    // LLM intermediate text
                                                    self.print_line(&cts.status);
                                                    self.push_entry(ScrollEntry::LlmText(cts.status.clone()));
                                                } else if cts.result_compact.is_none() {
                                                    // First status — tool is running (before execution)
                                                    let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                    let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                                                    let param_desc = cts.param_desc.as_deref().unwrap_or("");
                                                    let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Running);
                                                    let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
                                                    self.print_line(&header);
                                                    self.push_entry(ScrollEntry::ToolStatus(cts));
                                                } else {
                                                    // Second status — tool completed (after execution)
                                                    // Update matching ToolStatus entry in scroll_history
                                                    let tool_call_id = cts.tool_call_id.clone();
                                                    if let Some(entry) = self.scroll_history.iter_mut().rev().find(|e| {
                                                        matches!(e, ScrollEntry::ToolStatus(prev)
                                                            if prev.tool_call_id == tool_call_id)
                                                    }) {
                                                        *entry = ScrollEntry::ToolStatus(cts.clone());
                                                    }
                                                    // Re-render terminal: overwrite the Running header with updated icon
                                                    write_stdout("\x1b[1A\r\x1b[K");
                                                    let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                    let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                                                    let param_desc = cts.param_desc.as_deref().unwrap_or("");
                                                    let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                                                    let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
                                                    self.print_line(&header);
                                                    // Render result_compact with ⎿ gutter
                                                    if let Some(ref lines) = cts.result_compact {
                                                        let rendered = display::render_tool_output(lines);
                                                        for line in &rendered {
                                                            self.print_line(line);
                                                        }
                                                        if lines.len() < cts.result_full.as_ref().map_or(0, |f| f.len()) {
                                                            let total = cts.result_full.as_ref().unwrap().len();
                                                            self.print_line(&format!("  \x1b[2m   … +{} lines\x1b[0m", total - lines.len()));
                                                        }
                                                    }
                                                }
                                            }
                                            Some(Message::ChatToolCall(tc)) => {
                                                tool_calls.push(tc);
                                            }
                                            Some(Message::ChatResponse(resp)) if resp.request_id == req_id => {
                                                self.erase_thinking();
                                                self.print_line("");
                                                let rendered = markdown::render(&resp.content);
                                                let rendered = format!("\x1b[97m●\x1b[0m {}", rendered);
                                                for line in rendered.split("\r\n") {
                                                    self.print_line(line);
                                                }
                                                self.push_entry(ScrollEntry::Response(resp.content.clone()));
                                                let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                self.print_line(&display::render_separator(cols));
                                                self.push_entry(ScrollEntry::Separator);
                                                got_response = true;
                                                break;
                                            }
                                            None => break,
                                            _ => { got_response = true; break; }
                                        }
                                    }
                                    _ = wait_cancel(&mut crx) => {
                                        interrupted = true;
                                        break 'stream;
                                    }
                                }
                            }

                            if got_response || tool_calls.is_empty() {
                                break 'stream;
                            }

                            // Phase 2: Execute tools in parallel
                            let shell_cwd = super::get_shell_cwd(proxy.child_pid() as u32);
                            let mut handles = Vec::with_capacity(tool_calls.len());
                            for tc in &tool_calls {
                                let plugins = Arc::clone(&self.client_plugins);
                                let tool_name = tc.tool_name.clone();
                                let plugin_name = tc.plugin_name.clone();
                                let sandboxed = tc.sandboxed;
                                let tool_input: serde_json::Value =
                                    serde_json::from_str(&tc.input).unwrap_or_default();
                                let cwd = shell_cwd.clone();
                                handles.push(tokio::task::spawn_blocking(move || {
                                    plugins.execute_tool(
                                        &plugin_name,
                                        &tool_name,
                                        &tool_input,
                                        cwd.as_deref(),
                                        sandboxed,
                                    )
                                }));
                            }

                            // Wait for tools, race against Ctrl-C
                            let results;
                            {
                                let mut crx2 = cancel_rx.clone();
                                tokio::select! {
                                    all = async {
                                        let mut out = Vec::with_capacity(handles.len());
                                        for h in handles { out.push(h.await); }
                                        out
                                    } => { results = all; }
                                    _ = wait_cancel(&mut crx2) => {
                                        interrupted = true;
                                        break 'stream;
                                    }
                                }
                            }

                            // Phase 3: Send results (output rendered when second ChatToolStatus arrives)
                            let total = results.len();
                            let mut send_failed = false;
                            for (i, (tc, result)) in
                                tool_calls.iter().zip(results).enumerate()
                            {
                                let (content, is_error) = result
                                    .unwrap_or_else(|_| ("Tool execution panicked".to_string(), true));

                                let result_msg =
                                    Message::ChatToolResult(ChatToolResult {
                                        request_id: tc.request_id.clone(),
                                        thread_id: tc.thread_id.clone(),
                                        tool_call_id: tc.tool_call_id.clone(),
                                        content,
                                        is_error,
                                    });

                                if i < total - 1 {
                                    if let Err(e) = rpc.call(result_msg).await {
                                        write_stdout(&display::render_error(&format!(
                                            "Failed to send tool result: {}",
                                            e
                                        )));
                                        send_failed = true;
                                        break;
                                    }
                                } else {
                                    match rpc.call_stream(result_msg).await {
                                        Ok(new_rx) => {
                                            rx = new_rx;
                                            continue 'stream;
                                        }
                                        Err(e) => {
                                            write_stdout(&display::render_error(&format!(
                                                "Failed to send tool result: {}",
                                                e
                                            )));
                                            send_failed = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            if send_failed {
                                break 'stream;
                            }
                        }
                    }
                    Err(_) => {
                        write_stdout(&display::render_error("Failed to receive chat response"));
                    }
                }
            }

            // Stop Ctrl-C listener
            let _ = stop_tx.send(());

            if interrupted {
                self.erase_thinking();
                self.print_line("\x1b[2;37m(interrupted)\x1b[0m");
                self.push_entry(ScrollEntry::SystemMessage("(interrupted)".to_string()));

                let interrupt_msg = Message::ChatInterrupt(omnish_protocol::message::ChatInterrupt {
                    request_id: req_id.clone(),
                    session_id: session_id.to_string(),
                    thread_id: self.current_thread_id.clone().unwrap(),
                    query: trimmed.to_string(),
                });
                let rpc_clone = rpc.clone();
                tokio::spawn(async move {
                    let _ = rpc_clone.call(interrupt_msg).await;
                });
            }
        }
    }

    // ── Command handlers ─────────────────────────────────────────────────

    async fn handle_thread_del(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) {
        let idx_str = trimmed
            .strip_prefix("/thread del")
            .map(|s| s.trim())
            .unwrap_or("");

        let del_index = if idx_str.is_empty() {
            let rid = Uuid::new_v4().to_string()[..8].to_string();
            let req = Message::Request(Request {
                request_id: rid.clone(),
                session_id: session_id.to_string(),
                query: "__cmd:conversations".to_string(),
                scope: RequestScope::AllSessions,
            });
            match rpc.call(req).await {
                Ok(Message::Response(resp)) if resp.request_id == rid => {
                    if let Some(json) = super::parse_cmd_response(&resp.content) {
                        if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                            self.cached_thread_ids = ids
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                        let display_text = super::cmd_display_str(&json);
                        if self.cached_thread_ids.is_empty() {
                            return;
                        }
                        let item_strings: Vec<String> = display_text
                            .lines()
                            .filter(|l| l.trim_start().starts_with('['))
                            .map(|l| l.trim_start().to_string())
                            .collect();
                        let items: Vec<&str> = item_strings.iter().map(|s| s.as_str()).collect();
                        if items.is_empty() {
                            return;
                        }
                        match widgets::picker::pick_many("Select conversations to delete:", &items)
                        {
                            Some(mut indices) if !indices.is_empty() => {
                                indices.sort();
                                indices
                                    .iter()
                                    .map(|&i| (i + 1).to_string())
                                    .collect::<Vec<_>>()
                                    .join(",")
                            }
                            _ => return,
                        }
                    } else {
                        return;
                    }
                }
                _ => {
                    write_stdout(&display::render_error("Failed to list conversations"));
                    return;
                }
            }
        } else {
            idx_str.to_string()
        };

        // Auto-fetch if cache empty
        if self.cached_thread_ids.is_empty() {
            let rid = Uuid::new_v4().to_string()[..8].to_string();
            let req = Message::Request(Request {
                request_id: rid.clone(),
                session_id: session_id.to_string(),
                query: "__cmd:conversations".to_string(),
                scope: RequestScope::AllSessions,
            });
            if let Ok(Message::Response(resp)) = rpc.call(req).await {
                if resp.request_id == rid {
                    if let Some(json) = super::parse_cmd_response(&resp.content) {
                        if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                            self.cached_thread_ids = ids
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                    }
                }
            }
        }

        match super::parse_index_expr(&del_index) {
            Some(indices) => {
                let mut valid = true;
                for &i in &indices {
                    if i > self.cached_thread_ids.len() {
                        write_stdout(&display::render_error(&format!(
                            "Index {} out of range ({} conversations)",
                            i,
                            self.cached_thread_ids.len()
                        )));
                        valid = false;
                        break;
                    }
                }
                if valid {
                    let mut deleted = Vec::new();
                    for &i in &indices {
                        let tid = &self.cached_thread_ids[i - 1];
                        if tid.is_empty() {
                            write_stdout(&display::render_error(&format!(
                                "Conversation [{}] already deleted",
                                i
                            )));
                            continue;
                        }
                        let rid = Uuid::new_v4().to_string()[..8].to_string();
                        let req = Message::Request(Request {
                            request_id: rid.clone(),
                            session_id: session_id.to_string(),
                            query: format!("__cmd:conversations del {}", tid),
                            scope: RequestScope::AllSessions,
                        });
                        match rpc.call(req).await {
                            Ok(Message::Response(resp)) if resp.request_id == rid => {
                                if let Some(json) = super::parse_cmd_response(&resp.content) {
                                    if let Some(deleted_id) =
                                        json.get("deleted_thread_id").and_then(|v| v.as_str())
                                    {
                                        if self.current_thread_id.as_deref() == Some(deleted_id) {
                                            self.current_thread_id = None;
                                        }
                                    }
                                    self.cached_thread_ids[i - 1] = String::new();
                                    deleted.push(i);
                                }
                            }
                            _ => {
                                write_stdout(&display::render_error(&format!(
                                    "Failed to delete conversation [{}]",
                                    i
                                )));
                            }
                        }
                    }
                    if !deleted.is_empty() {
                        let nums: Vec<String> = deleted.iter().map(|i| format!("[{}]", i)).collect();
                        let msg = format!("Deleted conversation {}", nums.join(", "));
                        write_stdout(&display::render_response(&msg));
                    }
                }
            }
            None => {
                write_stdout(&display::render_error(
                    "Invalid index expression (use N, 1,2,3 or 1-3,5)",
                ));
            }
        }
    }

    async fn handle_thread_list(&mut self, session_id: &str, rpc: &RpcClient) {
        let request_id = Uuid::new_v4().to_string()[..8].to_string();
        let request = Message::Request(Request {
            request_id: request_id.clone(),
            session_id: session_id.to_string(),
            query: "__cmd:conversations".to_string(),
            scope: RequestScope::AllSessions,
        });
        match rpc.call(request).await {
            Ok(Message::Response(resp)) if resp.request_id == request_id => {
                if let Some(json) = super::parse_cmd_response(&resp.content) {
                    if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                        self.cached_thread_ids = ids
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                    }
                    let display_text = super::cmd_display_str(&json);
                    write_stdout(&display::render_response(&display_text));
                } else {
                    write_stdout(&display::render_response(&resp.content));
                }
            }
            _ => {
                write_stdout(&display::render_error("Failed to list conversations"));
            }
        }
    }

    async fn handle_resume(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) {
        // Returns (thread_id, response_json) where response_json contains structured history
        let (thread_id, response_json): (Option<String>, Option<serde_json::Value>) =
            if let Some(idx_str) = trimmed.strip_prefix("/resume ") {
            // Auto-fetch if cache empty
            if self.cached_thread_ids.is_empty() {
                let rid = Uuid::new_v4().to_string()[..8].to_string();
                let req = Message::Request(Request {
                    request_id: rid.clone(),
                    session_id: session_id.to_string(),
                    query: "__cmd:conversations".to_string(),
                    scope: RequestScope::AllSessions,
                });
                if let Ok(Message::Response(resp)) = rpc.call(req).await {
                    if resp.request_id == rid {
                        if let Some(json) = super::parse_cmd_response(&resp.content) {
                            if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                                self.cached_thread_ids = ids
                                    .iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect();
                            }
                        }
                    }
                }
            }
            match idx_str.trim().parse::<usize>() {
                Ok(i) if i >= 1 && i <= self.cached_thread_ids.len() => {
                    let tid = self.cached_thread_ids[i - 1].clone();
                    if tid.is_empty() {
                        write_stdout(&display::render_error(&format!(
                            "Conversation [{}] was deleted",
                            i
                        )));
                        (None, None)
                    } else {
                        let rid = Uuid::new_v4().to_string()[..8].to_string();
                        let req = Message::Request(Request {
                            request_id: rid.clone(),
                            session_id: session_id.to_string(),
                            query: format!("__cmd:resume_tid {}", tid),
                            scope: RequestScope::AllSessions,
                        });
                        let resp_json = match rpc.call(req).await {
                            Ok(Message::Response(resp)) if resp.request_id == rid => {
                                super::parse_cmd_response(&resp.content)
                            }
                            _ => None,
                        };
                        (Some(tid), resp_json)
                    }
                }
                Ok(i) if i >= 1 => {
                    write_stdout(&display::render_error(&format!(
                        "Index {} out of range ({} conversations)",
                        i,
                        self.cached_thread_ids.len()
                    )));
                    (None, None)
                }
                _ => {
                    write_stdout(&display::render_error("Invalid index"));
                    (None, None)
                }
            }
        } else {
            // /resume without index — picker
            let rid = Uuid::new_v4().to_string()[..8].to_string();
            let req = Message::Request(Request {
                request_id: rid.clone(),
                session_id: session_id.to_string(),
                query: "__cmd:conversations".to_string(),
                scope: RequestScope::AllSessions,
            });
            match rpc.call(req).await {
                Ok(Message::Response(resp)) if resp.request_id == rid => {
                    if let Some(json) = super::parse_cmd_response(&resp.content) {
                        if let Some(ids) = json.get("thread_ids").and_then(|v| v.as_array()) {
                            self.cached_thread_ids = ids
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                        if self.cached_thread_ids.is_empty() {
                            write_stdout(&display::render_error("No conversations to resume"));
                            (None, None)
                        } else {
                            let display_str = super::cmd_display_str(&json);
                            let item_strings: Vec<String> = display_str
                                .lines()
                                .filter(|l| l.trim_start().starts_with('['))
                                .map(|l| l.trim_start().to_string())
                                .collect();
                            let items: Vec<&str> =
                                item_strings.iter().map(|s| s.as_str()).collect();
                            if items.is_empty() {
                                write_stdout(&display::render_error("No conversations to resume"));
                                (None, None)
                            } else {
                                match widgets::picker::pick_one("Resume conversation:", &items) {
                                    Some(idx) if idx < self.cached_thread_ids.len() => {
                                        let tid = self.cached_thread_ids[idx].clone();
                                        let rid2 = Uuid::new_v4().to_string()[..8].to_string();
                                        let req2 = Message::Request(Request {
                                            request_id: rid2.clone(),
                                            session_id: session_id.to_string(),
                                            query: format!("__cmd:resume_tid {}", tid),
                                            scope: RequestScope::AllSessions,
                                        });
                                        let resp_json = match rpc.call(req2).await {
                                            Ok(Message::Response(r)) if r.request_id == rid2 => {
                                                super::parse_cmd_response(&r.content)
                                            }
                                            _ => None,
                                        };
                                        (Some(tid), resp_json)
                                    }
                                    _ => (None, None),
                                }
                            }
                        }
                    } else {
                        write_stdout(&display::render_error("No conversations to resume"));
                        (None, None)
                    }
                }
                _ => {
                    write_stdout(&display::render_error("Failed to list conversations"));
                    (None, None)
                }
            }
        };

        if let Some(tid) = thread_id {
            self.current_thread_id = Some(tid);
            if let Some(history) = response_json.as_ref().and_then(|j| j.get("history")).and_then(|h| h.as_array()) {
                // Parse structured history entries
                let mut all_entries: Vec<ScrollEntry> = Vec::new();
                for entry in history {
                    match entry.get("type").and_then(|t| t.as_str()) {
                        Some("user_input") => {
                            let text = entry["text"].as_str().unwrap_or("");
                            all_entries.push(ScrollEntry::UserInput(text.to_string()));
                        }
                        Some("llm_text") => {
                            let text = entry["text"].as_str().unwrap_or("");
                            all_entries.push(ScrollEntry::LlmText(text.to_string()));
                        }
                        Some("tool_status") => {
                            let cts = ChatToolStatus {
                                request_id: String::new(),
                                thread_id: String::new(),
                                tool_name: entry["tool_name"].as_str().unwrap_or("").to_string(),
                                tool_call_id: entry["tool_call_id"].as_str().map(String::from),
                                status: String::new(),
                                status_icon: Some(match entry["status_icon"].as_str() {
                                    Some("error") => StatusIcon::Error,
                                    Some("running") => StatusIcon::Running,
                                    _ => StatusIcon::Success,
                                }),
                                display_name: entry["display_name"].as_str().map(String::from),
                                param_desc: entry["param_desc"].as_str().map(String::from),
                                result_compact: entry["result_compact"].as_array().map(|a|
                                    a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
                                result_full: entry["result_full"].as_array().map(|a|
                                    a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
                            };
                            all_entries.push(ScrollEntry::ToolStatus(cts));
                        }
                        Some("response") => {
                            let text = entry["text"].as_str().unwrap_or("");
                            all_entries.push(ScrollEntry::Response(text.to_string()));
                        }
                        Some("separator") => {
                            all_entries.push(ScrollEntry::Separator);
                        }
                        _ => {}
                    }
                }

                // Find the start of the last exchange (last UserInput)
                let last_exchange_start = all_entries.iter().rposition(|e|
                    matches!(e, ScrollEntry::UserInput(_))
                ).unwrap_or(0);

                // Push ALL entries to scroll_history (for Ctrl+O browse)
                for entry in &all_entries {
                    self.push_entry(entry.clone());
                }

                // Render only the last exchange on terminal
                let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                // Show count of earlier entries if any
                if last_exchange_start > 0 {
                    let earlier_count = all_entries[..last_exchange_start].iter()
                        .filter(|e| matches!(e, ScrollEntry::UserInput(_)))
                        .count();
                    if earlier_count > 0 {
                        self.print_line(&format!(
                            "\x1b[2;37m({} earlier exchange{})\x1b[0m",
                            earlier_count,
                            if earlier_count == 1 { "" } else { "s" }
                        ));
                    }
                }
                for entry in &all_entries[last_exchange_start..] {
                    match entry {
                        ScrollEntry::UserInput(text) => {
                            for (i, line) in text.lines().enumerate() {
                                if i == 0 {
                                    self.print_line(&format!("\x1b[36m> \x1b[0m{}", line));
                                } else {
                                    self.print_line(&format!("  {}", line));
                                }
                            }
                        }
                        ScrollEntry::ToolStatus(cts) => {
                            let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                            let param_desc = cts.param_desc.as_deref().unwrap_or("");
                            let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                            let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
                            self.print_line(&header);
                            if let Some(ref lines) = cts.result_compact {
                                let rendered = display::render_tool_output(lines);
                                for line in &rendered {
                                    self.print_line(line);
                                }
                            }
                        }
                        ScrollEntry::LlmText(text) => {
                            self.print_line(text);
                        }
                        ScrollEntry::Response(content) => {
                            self.print_line("");
                            let rendered = markdown::render(content);
                            let rendered = format!("\x1b[97m●\x1b[0m {}", rendered);
                            for line in rendered.split("\r\n") {
                                self.print_line(line);
                            }
                        }
                        ScrollEntry::Separator => {
                            self.print_line(&display::render_separator(cols));
                        }
                        ScrollEntry::SystemMessage(msg) => {
                            self.print_line(&format!("\x1b[2;37m{}\x1b[0m", msg));
                        }
                    }
                }
            } else {
                write_stdout("\x1b[2;37m(resumed conversation)\x1b[0m\r\n");
                self.push_entry(ScrollEntry::SystemMessage("(resumed conversation)".to_string()));
            }
        }
    }

    // ── Input handling ───────────────────────────────────────────────────

    fn read_input(&mut self, allow_backspace_exit: bool) -> Option<String> {
        use unicode_width::UnicodeWidthChar;
        use widgets::line_editor::LineEditor;

        let stdin_fd = std::io::stdin().as_raw_fd();
        let mut editor = LineEditor::new();
        let mut byte = [0u8; 1];
        let mut has_ghost = false;
        let mut ghost_text = String::new();
        let mut bracketed_paste = false;
        let mut last_input = std::time::Instant::now();

        struct PasteBlock {
            content: String,
            index: usize,
            line_count: usize,
        }
        let paste_blocks: std::cell::RefCell<Vec<PasteBlock>> = std::cell::RefCell::new(vec![]);
        let mut paste_count = 0usize;
        let mut paste_buf = String::new();
        let mut paste_buffering = false;
        let mut paste_last_cr = false;

        // Enable bracketed paste
        write_stdout("\x1b[?2004h");

        let term_cursor_row = std::cell::Cell::new(0usize);

        // Redraw closure — relative cursor movement, no layout dependency
        let redraw = |editor: &LineEditor, ghost: &str, has_ghost: bool| {
            let blocks = paste_blocks.borrow();
            let line_count = editor.line_count();
            let (cursor_row, cursor_col) = editor.cursor();
            let mut fffc_idx = 0usize;
            let mut out = String::new();

            let prev_row = term_cursor_row.get();
            if prev_row > 0 {
                out.push_str(&format!("\x1b[{}A", prev_row));
            }
            out.push('\r');

            let cols = super::get_terminal_size().unwrap_or((24, 80)).1 as usize;
            let cols = cols.max(1);

            let mut display_widths = Vec::with_capacity(line_count);
            for i in 0..line_count {
                let line = editor.line(i);
                let pfx = if i == 0 { "\x1b[36m> \x1b[0m" } else { "  " };
                let mut s = String::new();
                s.push_str(pfx);
                let mut dw = 2usize;

                let has_fffc = line.contains(&'\u{FFFC}');
                if has_fffc {
                    for &ch in line {
                        if ch == '\u{FFFC}' {
                            if let Some(block) = blocks.get(fffc_idx) {
                                let marker = format!(
                                    "[pasted text #{} +{} lines]",
                                    block.index, block.line_count
                                );
                                dw += marker.len();
                                s.push_str(&format!("\x1b[2;36m{}\x1b[0m", marker));
                            }
                            fffc_idx += 1;
                        } else {
                            dw += UnicodeWidthChar::width(ch).unwrap_or(1);
                            s.push(ch);
                        }
                    }
                } else {
                    for &ch in line {
                        dw += UnicodeWidthChar::width(ch).unwrap_or(1);
                    }
                    let line_str: String = line.iter().collect();
                    s.push_str(&line_str);
                }

                let cursor_on_fffc = line.contains(&'\u{FFFC}');
                if i == line_count - 1 && has_ghost && !ghost.is_empty() && !cursor_on_fffc {
                    for ch in ghost.chars() {
                        dw += UnicodeWidthChar::width(ch).unwrap_or(1);
                    }
                    s.push_str(&format!("\x1b[2;37m{}\x1b[0m", ghost));
                }

                if i == line_count - 1 {
                    out.push_str(&s);
                    out.push_str("\x1b[J");
                } else {
                    out.push_str(&s);
                    out.push_str("\x1b[K\r\n");
                }
                display_widths.push(dw);
            }

            // Cursor positioning
            let mut cursor_display = 2usize;
            let cursor_line = editor.line(cursor_row);
            let mut local_fffc = 0usize;
            let fffc_before_cursor_row: usize = (0..cursor_row)
                .map(|r| editor.line(r).iter().filter(|&&c| c == '\u{FFFC}').count())
                .sum();
            for &ch in &cursor_line[..cursor_col] {
                if ch == '\u{FFFC}' {
                    let block_idx = fffc_before_cursor_row + local_fffc;
                    if let Some(block) = blocks.get(block_idx) {
                        cursor_display += format!(
                            "[pasted text #{} +{} lines]",
                            block.index, block.line_count
                        )
                        .len();
                    }
                    local_fffc += 1;
                } else {
                    cursor_display += UnicodeWidthChar::width(ch).unwrap_or(1);
                }
            }

            let cursor_after_visual_row: usize = {
                let mut r = 0;
                for w in display_widths.iter().take(line_count.saturating_sub(1)) {
                    r += w / cols + 1;
                }
                r += display_widths[line_count - 1] / cols;
                r
            };
            let target_visual_row: usize = {
                let mut r = 0;
                for w in display_widths.iter().take(cursor_row) {
                    r += w / cols + 1;
                }
                r += cursor_display / cols;
                r
            };
            let target_visual_col = cursor_display % cols;

            let rows_up = cursor_after_visual_row.saturating_sub(target_visual_row);
            if rows_up > 0 {
                out.push_str(&format!("\x1b[{}A", rows_up));
            }
            out.push('\r');
            if target_visual_col > 0 {
                out.push_str(&format!("\x1b[{}C", target_visual_col));
            }

            term_cursor_row.set(target_visual_row);
            write_stdout(&out);
        };

        let disable_paste = || {
            write_stdout("\x1b[?2004l");
        };

        loop {
            // Paste buffer finalization on timeout
            if paste_buffering && !bracketed_paste {
                let mut pfd =
                    libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
                if unsafe { libc::poll(&mut pfd, 1, 2) } <= 0 {
                    paste_buffering = false;
                    let line_count = paste_buf.lines().count()
                        + if paste_buf.ends_with('\n') { 1 } else { 0 };
                    let line_count = line_count.max(if paste_buf.is_empty() { 0 } else { 1 });
                    if line_count >= 10 {
                        paste_count += 1;
                        paste_blocks.borrow_mut().push(PasteBlock {
                            content: paste_buf.clone(),
                            index: paste_count,
                            line_count,
                        });
                        editor.insert_paste_block();
                    } else if !paste_buf.is_empty() {
                        for ch in paste_buf.chars() {
                            if ch == '\n' {
                                editor.newline();
                            } else {
                                editor.insert(ch);
                            }
                        }
                    }
                    paste_buf.clear();
                    has_ghost = false;
                    ghost_text.clear();
                    self.completer.clear();
                    redraw(&editor, "", false);
                }
            }

            match nix::unistd::read(stdin_fd, &mut byte) {
                Ok(1) => {
                    let now = std::time::Instant::now();
                    let backward = now.duration_since(last_input).as_millis() < 1;
                    last_input = now;
                    let mut pfd =
                        libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
                    let forward = unsafe { libc::poll(&mut pfd, 1, 0) } > 0;
                    let pasting = bracketed_paste || backward || forward;

                    if pasting && !paste_buffering && byte[0] != 0x1b {
                        paste_buffering = true;
                        paste_buf.clear();
                        paste_last_cr = false;
                    }

                    if !pasting && paste_buffering {
                        paste_buffering = false;
                        let line_count = paste_buf.lines().count()
                            + if paste_buf.ends_with('\n') { 1 } else { 0 };
                        let line_count =
                            line_count.max(if paste_buf.is_empty() { 0 } else { 1 });
                        if line_count >= 10 {
                            paste_count += 1;
                            paste_blocks.borrow_mut().push(PasteBlock {
                                content: paste_buf.clone(),
                                index: paste_count,
                                line_count,
                            });
                            editor.insert_paste_block();
                        } else if !paste_buf.is_empty() {
                            for ch in paste_buf.chars() {
                                if ch == '\n' {
                                    editor.newline();
                                } else {
                                    editor.insert(ch);
                                }
                            }
                        }
                        paste_buf.clear();
                        has_ghost = false;
                        ghost_text.clear();
                        self.completer.clear();
                        redraw(&editor, "", false);
                    }

                    match byte[0] {
                        0x1b => match parse_key_after_esc(stdin_fd) {
                            Some(KeyEvent::Esc) => {
                                disable_paste();
                                return None;
                            }
                            Some(KeyEvent::PasteStart) => {
                                bracketed_paste = true;
                            }
                            Some(KeyEvent::PasteEnd) => {
                                bracketed_paste = false;
                                paste_buffering = false;
                                let line_count = paste_buf.lines().count()
                                    + if paste_buf.ends_with('\n') { 1 } else { 0 };
                                let line_count =
                                    line_count.max(if paste_buf.is_empty() { 0 } else { 1 });
                                if line_count >= 10 {
                                    paste_count += 1;
                                    paste_blocks.borrow_mut().push(PasteBlock {
                                        content: paste_buf.clone(),
                                        index: paste_count,
                                        line_count,
                                    });
                                    editor.insert_paste_block();
                                } else if !paste_buf.is_empty() {
                                    for ch in paste_buf.chars() {
                                        if ch == '\n' {
                                            editor.newline();
                                        } else {
                                            editor.insert(ch);
                                        }
                                    }
                                }
                                paste_buf.clear();
                                has_ghost = false;
                                ghost_text.clear();
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                            Some(KeyEvent::ShiftEnter) => {
                                editor.newline();
                                has_ghost = false;
                                ghost_text.clear();
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                            Some(KeyEvent::ArrowUp) => {
                                if editor.is_empty() || self.history_index.is_some() {
                                    if self.chat_history.is_empty() {
                                        continue;
                                    }
                                    let idx = match self.history_index {
                                        Some(i) if i > 0 => i - 1,
                                        Some(_) => continue,
                                        None => self.chat_history.len() - 1,
                                    };
                                    self.history_index = Some(idx);
                                    if let Some(cmd) = self.chat_history.get(idx) {
                                        let cmd = cmd.clone();
                                        editor.set_content(&cmd);
                                        has_ghost = false;
                                        ghost_text.clear();
                                        if let Some(g) = self.completer.update(&cmd) {
                                            ghost_text = g.to_string();
                                            has_ghost = true;
                                            redraw(&editor, &ghost_text, true);
                                        } else {
                                            redraw(&editor, "", false);
                                        }
                                    }
                                } else {
                                    editor.move_up();
                                    redraw(&editor, &ghost_text, has_ghost);
                                }
                            }
                            Some(KeyEvent::ArrowDown) => {
                                if editor.is_empty() || self.history_index.is_some() {
                                    if self.chat_history.is_empty() {
                                        continue;
                                    }
                                    let idx = match self.history_index {
                                        Some(i) if i < self.chat_history.len() - 1 => i + 1,
                                        Some(_) => {
                                            self.history_index = None;
                                            editor.set_content("");
                                            has_ghost = false;
                                            ghost_text.clear();
                                            self.completer.clear();
                                            redraw(&editor, "", false);
                                            continue;
                                        }
                                        None => continue,
                                    };
                                    self.history_index = Some(idx);
                                    if let Some(cmd) = self.chat_history.get(idx) {
                                        let cmd = cmd.clone();
                                        editor.set_content(&cmd);
                                        has_ghost = false;
                                        ghost_text.clear();
                                        if let Some(g) = self.completer.update(&cmd) {
                                            ghost_text = g.to_string();
                                            has_ghost = true;
                                            redraw(&editor, &ghost_text, true);
                                        } else {
                                            redraw(&editor, "", false);
                                        }
                                    }
                                } else {
                                    editor.move_down();
                                    redraw(&editor, &ghost_text, has_ghost);
                                }
                            }
                            Some(KeyEvent::ArrowLeft) => {
                                editor.move_left();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::ArrowRight) => {
                                editor.move_right();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::Home) => {
                                editor.move_home();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::End) => {
                                editor.move_end();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::Delete) => {
                                editor.delete_forward();
                                has_ghost = false;
                                ghost_text.clear();
                                let content = editor.content();
                                if let Some(g) = self.completer.update(&content) {
                                    ghost_text = g.to_string();
                                    has_ghost = true;
                                    redraw(&editor, &ghost_text, true);
                                } else {
                                    self.completer.clear();
                                    redraw(&editor, "", false);
                                }
                            }
                            Some(KeyEvent::CtrlLeft) => {
                                editor.move_word_left();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            Some(KeyEvent::CtrlRight) => {
                                editor.move_word_right();
                                redraw(&editor, &ghost_text, has_ghost);
                            }
                            None => {}
                        },
                        _ if paste_buffering => {
                            match byte[0] {
                                0x0d => {
                                    paste_buf.push('\n');
                                    paste_last_cr = true;
                                }
                                0x0a => {
                                    if !paste_last_cr {
                                        paste_buf.push('\n');
                                    }
                                    paste_last_cr = false;
                                }
                                b if (0x20..0x80).contains(&b) => {
                                    paste_last_cr = false;
                                    paste_buf.push(b as char);
                                }
                                b if b >= 0x80 => {
                                    paste_last_cr = false;
                                    let mut utf8_buf = vec![b];
                                    let expected =
                                        if b < 0xE0 { 1 } else if b < 0xF0 { 2 } else { 3 };
                                    for _ in 0..expected {
                                        if nix::unistd::read(stdin_fd, &mut byte).unwrap_or(0) == 1
                                        {
                                            utf8_buf.push(byte[0]);
                                        }
                                    }
                                    let ch = String::from_utf8_lossy(&utf8_buf)
                                        .chars()
                                        .next()
                                        .unwrap_or('?');
                                    paste_buf.push(ch);
                                }
                                _ => {
                                    paste_last_cr = false;
                                }
                            }
                            continue;
                        }
                        0x0f => {
                            // Ctrl-O — browse history (alternate screen)
                            self.browse_history();
                        }
                        0x01 => {
                            editor.move_home();
                            redraw(&editor, &ghost_text, has_ghost);
                        }
                        0x05 => {
                            editor.move_end();
                            redraw(&editor, &ghost_text, has_ghost);
                        }
                        0x15 => {
                            editor.kill_to_start();
                            has_ghost = false;
                            ghost_text.clear();
                            let content = editor.content();
                            if let Some(g) = self.completer.update(&content) {
                                ghost_text = g.to_string();
                                has_ghost = true;
                                redraw(&editor, &ghost_text, true);
                            } else {
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                        }
                        0x04 if editor.is_empty() && paste_blocks.borrow().is_empty() => {
                            disable_paste();
                            return None;
                        }
                        0x0a => {
                            editor.newline();
                            has_ghost = false;
                            ghost_text.clear();
                            self.completer.clear();
                            redraw(&editor, "", false);
                        }
                        0x0d => {
                            // Enter — submit
                            if has_ghost {
                                redraw(&editor, "", false);
                            }
                            let last_row = editor.line_count() - 1;
                            let (cur_row, _) = editor.cursor();
                            if cur_row < last_row {
                                let down = last_row - cur_row;
                                write_stdout(&format!("\x1b[{}B", down));
                            }
                            let blocks = paste_blocks.borrow();
                            let fffc_before_last: usize = (0..last_row)
                                .map(|r| {
                                    editor
                                        .line(r)
                                        .iter()
                                        .filter(|&&c| c == '\u{FFFC}')
                                        .count()
                                })
                                .sum();
                            let mut end_col = 2usize;
                            let mut local_fi = 0usize;
                            for &ch in editor.line(last_row) {
                                if ch == '\u{FFFC}' {
                                    let bi = fffc_before_last + local_fi;
                                    if let Some(b) = blocks.get(bi) {
                                        end_col += format!(
                                            "[pasted text #{} +{} lines]",
                                            b.index, b.line_count
                                        )
                                        .len();
                                    }
                                    local_fi += 1;
                                } else {
                                    end_col += UnicodeWidthChar::width(ch).unwrap_or(1);
                                }
                            }
                            drop(blocks);
                            write_stdout(&format!("\r\x1b[{}C", end_col));
                            self.completer.clear();
                            disable_paste();
                            // Assemble full content
                            let blocks = paste_blocks.borrow();
                            if blocks.is_empty() {
                                return Some(editor.content());
                            }
                            let mut full = String::new();
                            let mut block_idx = 0usize;
                            let lc = editor.line_count();
                            for i in 0..lc {
                                let line = editor.line(i);
                                let is_fffc_only = line.len() == 1 && line[0] == '\u{FFFC}';
                                if is_fffc_only {
                                    if let Some(block) = blocks.get(block_idx) {
                                        full.push_str(&block.content);
                                        if !block.content.ends_with('\n') {
                                            full.push('\n');
                                        }
                                        block_idx += 1;
                                    }
                                } else {
                                    let line_str: String =
                                        line.iter().filter(|&&c| c != '\u{FFFC}').collect();
                                    full.push_str(&line_str);
                                    if i < lc - 1 {
                                        full.push('\n');
                                    }
                                }
                            }
                            return Some(full);
                        }
                        0x09 => {
                            if let Some(suffix) = self.completer.accept() {
                                for ch in suffix.chars() {
                                    editor.insert(ch);
                                }
                                has_ghost = false;
                                ghost_text.clear();
                                let content = editor.content();
                                if let Some(g) = self.completer.update(&content) {
                                    ghost_text = g.to_string();
                                    has_ghost = true;
                                    redraw(&editor, &ghost_text, true);
                                } else {
                                    redraw(&editor, "", false);
                                }
                            }
                        }
                        0x7f | 0x08 => {
                            let (row, col) = editor.cursor();
                            if col > 0 && editor.line(row)[col - 1] == '\u{FFFC}' {
                                let fffc_idx: usize = (0..row)
                                    .map(|r| {
                                        editor
                                            .line(r)
                                            .iter()
                                            .filter(|&&c| c == '\u{FFFC}')
                                            .count()
                                    })
                                    .sum();
                                editor.delete_back();
                                let (nr, _) = editor.cursor();
                                if editor.line(nr).is_empty() && nr > 0 {
                                    editor.delete_back();
                                }
                                paste_blocks.borrow_mut().remove(fffc_idx);
                                has_ghost = false;
                                ghost_text.clear();
                                self.completer.clear();
                                redraw(&editor, "", false);
                                continue;
                            }
                            if editor.is_empty() && paste_blocks.borrow().is_empty() {
                                if allow_backspace_exit {
                                    disable_paste();
                                    return None;
                                }
                                continue;
                            }
                            if !editor.delete_back() {
                                continue;
                            }
                            has_ghost = false;
                            ghost_text.clear();
                            let content = editor.content();
                            if let Some(g) = self.completer.update(&content) {
                                ghost_text = g.to_string();
                                has_ghost = true;
                                redraw(&editor, &ghost_text, true);
                            } else {
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                        }
                        b if b >= 0x20 => {
                            let ch = if b < 0x80 {
                                b as char
                            } else {
                                let mut utf8_buf = vec![b];
                                let expected =
                                    if b < 0xE0 { 1 } else if b < 0xF0 { 2 } else { 3 };
                                for _ in 0..expected {
                                    if nix::unistd::read(stdin_fd, &mut byte).unwrap_or(0) == 1 {
                                        utf8_buf.push(byte[0]);
                                    }
                                }
                                String::from_utf8_lossy(&utf8_buf)
                                    .chars()
                                    .next()
                                    .unwrap_or('?')
                            };
                            editor.insert(ch);
                            has_ghost = false;
                            ghost_text.clear();
                            let content = editor.content();
                            if let Some(g) = self.completer.update(&content) {
                                ghost_text = g.to_string();
                                has_ghost = true;
                                redraw(&editor, &ghost_text, true);
                            } else {
                                self.completer.clear();
                                redraw(&editor, "", false);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {
                    disable_paste();
                    return None;
                }
            }
        }
    }
}

// ── Standalone helpers ───────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum KeyEvent {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Delete,
    CtrlLeft,
    CtrlRight,
    ShiftEnter,
    PasteStart,
    PasteEnd,
    Esc,
}

fn parse_key_after_esc(stdin_fd: i32) -> Option<KeyEvent> {
    let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut pfd, 1, 15) };
    if ready <= 0 {
        return Some(KeyEvent::Esc);
    }

    let mut b = [0u8; 1];
    if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
        return Some(KeyEvent::Esc);
    }

    match b[0] {
        b'[' => {}
        _ => return None,
    }

    let mut params = Vec::new();
    loop {
        if nix::unistd::read(stdin_fd, &mut b) != Ok(1) {
            return None;
        }
        if b[0] >= 0x40 && b[0] <= 0x7E {
            break;
        }
        params.push(b[0]);
    }
    let final_byte = b[0];

    match (params.as_slice(), final_byte) {
        ([], b'A') => Some(KeyEvent::ArrowUp),
        ([], b'B') => Some(KeyEvent::ArrowDown),
        ([], b'C') => Some(KeyEvent::ArrowRight),
        ([], b'D') => Some(KeyEvent::ArrowLeft),
        ([], b'H') => Some(KeyEvent::Home),
        ([], b'F') => Some(KeyEvent::End),
        ([b'3'], b'~') => Some(KeyEvent::Delete),
        ([b'1', b';', b'5'], b'C') => Some(KeyEvent::CtrlRight),
        ([b'1', b';', b'5'], b'D') => Some(KeyEvent::CtrlLeft),
        ([b'1'], b'~') => Some(KeyEvent::Home),
        ([b'4'], b'~') => Some(KeyEvent::End),
        ([b'1', b'3', b';', b'2'], b'u') => Some(KeyEvent::ShiftEnter),
        ([b'2', b'0', b'0'], b'~') => Some(KeyEvent::PasteStart),
        ([b'2', b'0', b'1'], b'~') => Some(KeyEvent::PasteEnd),
        _ => None,
    }
}

fn wait_for_ctrl_c(stop: std::sync::mpsc::Receiver<()>) -> bool {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut byte = [0u8; 1];
    loop {
        if stop.try_recv().is_ok() {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd: stdin_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ret <= 0 {
            continue;
        }
        match nix::unistd::read(stdin_fd, &mut byte) {
            Ok(1) if byte[0] == 0x03 => return true,
            Ok(1) => {}
            _ => return false,
        }
    }
}

fn save_to_history(history: &mut VecDeque<String>, command: &str, capacity: usize) {
    if command.trim().is_empty() || history.back().is_some_and(|s| s == command) {
        return;
    }
    if history.len() >= capacity {
        history.pop_front();
    }
    history.push_back(command.to_string());
}
