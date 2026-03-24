use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
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

/// Action requested by chat session upon exit.
pub enum ChatExitAction {
    /// Normal exit — no special action needed.
    Normal,
    /// Request to toggle Landlock sandbox on the shell process.
    Lock(bool),
}

enum ResumeMismatchAction {
    Cancel,
    CdToOld(String),
    StayHere(String),
    ContinueDifferentHost,
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
    ghost_hint_shown: bool,
    pending_model: Option<String>,
    /// Non-default model name for resumed thread (shown as ghost hint).
    resumed_model: Option<String>,
    /// Shell's current working directory (from /proc/pid/cwd), set at chat entry.
    shell_cwd: Option<String>,
    /// Directory to cd into after chat mode exits (set by resume mismatch handler).
    pending_cd: Option<String>,
    /// Total terminal lines printed (for tracking tool section position).
    lines_printed: usize,
    /// Line position where the current batch of tool headers starts.
    tool_section_start: Option<usize>,
    /// scroll_history index where the current tool batch starts.
    tool_section_hist_idx: Option<usize>,
}

fn write_stdout(s: &str) {
    nix::unistd::write(std::io::stdout(), s.as_bytes()).ok();
}

/// Strip `-YYYYMMDD` date suffix from model name for display.
/// e.g. "claude-sonnet-4-5-20250929" → "claude-sonnet-4-5"
fn strip_date_suffix(model: &str) -> &str {
    if model.len() > 9 {
        let suffix = &model[model.len() - 9..];
        if suffix.starts_with('-') && suffix[1..].bytes().all(|b| b.is_ascii_digit()) {
            return &model[..model.len() - 9];
        }
    }
    model
}

/// Convert path segment to display label: capitalize first letter, _ -> space.
fn segment_to_label(seg: &str) -> String {
    seg.replace('_', " ")
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.collect::<String>()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build MenuItem tree from flat ConfigItems and handler info.
fn build_menu_tree(
    items: &[ConfigItem],
    handlers: &[ConfigHandlerInfo],
) -> (Vec<widgets::menu::MenuItem>, HashMap<String, String>) {
    use widgets::menu::MenuItem;
    let mut root: Vec<MenuItem> = Vec::new();
    let mut path_map: HashMap<String, String> = HashMap::new();

    let handler_lookup: HashMap<&str, (&str, &str)> = handlers.iter()
        .map(|h| (h.path.as_str(), (h.handler.as_str(), h.label.as_str())))
        .collect();

    for item in items {
        let segments: Vec<&str> = item.path.split('.').collect();
        let mut current = &mut root;
        for (i, &seg) in segments.iter().enumerate() {
            if i == segments.len() - 1 {
                // Leaf item
                let menu_item = match &item.kind {
                    ConfigItemKind::Toggle { value } => MenuItem::Toggle {
                        label: item.label.clone(),
                        value: *value,
                    },
                    ConfigItemKind::Select { options, selected } => MenuItem::Select {
                        label: item.label.clone(),
                        options: options.clone(),
                        selected: *selected,
                    },
                    ConfigItemKind::TextInput { value } => MenuItem::TextInput {
                        label: item.label.clone(),
                        value: value.clone(),
                    },
                };
                current.push(menu_item);

                // Build display path for path_map reverse lookup
                let mut display_parts: Vec<String> = Vec::new();
                let mut schema_prefix = String::new();
                for (j, &s) in segments[..i].iter().enumerate() {
                    if j > 0 { schema_prefix.push('.'); }
                    schema_prefix.push_str(s);
                    let label = if s == "__new__" {
                        handler_lookup.get(schema_prefix.as_str())
                            .map(|(_, lbl)| lbl.to_string())
                            .unwrap_or_else(|| segment_to_label(s))
                    } else {
                        segment_to_label(s)
                    };
                    display_parts.push(label);
                }
                display_parts.push(item.label.clone());
                let display_key = display_parts.join(".");
                path_map.insert(display_key, item.path.clone());
            } else {
                // Intermediate segment — find or create submenu
                let schema_path_so_far = segments[..=i].join(".");
                let label = if seg == "__new__" {
                    handler_lookup.get(schema_path_so_far.as_str())
                        .map(|(_, lbl)| lbl.to_string())
                        .unwrap_or_else(|| segment_to_label(seg))
                } else {
                    segment_to_label(seg)
                };

                let pos = current.iter().position(|m| {
                    matches!(m, MenuItem::Submenu { label: l, .. } if *l == label)
                });
                let idx = match pos {
                    Some(idx) => idx,
                    None => {
                        let handler = handler_lookup.get(schema_path_so_far.as_str())
                            .map(|(name, _)| name.to_string());
                        current.push(MenuItem::Submenu {
                            label: label.clone(),
                            children: Vec::new(),
                            handler,
                        });
                        current.len() - 1
                    }
                };
                current = match &mut current[idx] {
                    MenuItem::Submenu { children, .. } => children,
                    _ => unreachable!(),
                };
            }
        }
    }

    (root, path_map)
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
            ghost_hint_shown: false,
            pending_model: None,
            resumed_model: None,
            shell_cwd: None,
            pending_cd: None,
            lines_printed: 0,
            tool_section_start: None,
            tool_section_hist_idx: None,
        }
    }

    pub fn into_history(self) -> VecDeque<String> {
        self.chat_history
    }

    /// Return the thread ID used in this chat session (if any).
    pub fn thread_id(&self) -> Option<&str> {
        self.current_thread_id.as_deref()
    }

    /// Return a pending cd path set by resume mismatch handler.
    pub fn pending_cd(&self) -> Option<&str> {
        self.pending_cd.as_deref()
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
        let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
        self.lines_printed += Self::visual_rows(line, cols as usize);
    }

    /// How many terminal rows a line occupies (accounting for wrapping).
    fn visual_rows(line: &str, cols: usize) -> usize {
        let w = display::display_width(line);
        if w == 0 || cols == 0 { 1 } else { ((w - 1) / cols) + 1 }
    }

    /// Re-render the tool section from tool_section_start.
    /// Moves cursor up, erases, and re-renders all ToolStatus entries with their output.
    fn redraw_tool_section(&mut self) {
        let start_line = match self.tool_section_start {
            Some(s) => s,
            None => return,
        };
        let hist_start = match self.tool_section_hist_idx {
            Some(s) => s,
            None => return,
        };

        let lines_up = self.lines_printed - start_line;
        if lines_up > 0 {
            write_stdout(&format!("\x1b[{}A", lines_up));
        }
        write_stdout("\r\x1b[J"); // erase from cursor to end of screen

        let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
        let cols = cols as usize;
        let mut count = 0usize;

        for entry in &self.scroll_history[hist_start..] {
            if let ScrollEntry::ToolStatus(cts) = entry {
                let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                let param_desc = cts.param_desc.as_deref().unwrap_or("");
                let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Running);
                let header = display::render_tool_header(icon, display_name, param_desc, cols);
                write_stdout(&header);
                write_stdout("\r\n");
                count += Self::visual_rows(&header, cols);
                if let Some(ref lines) = cts.result_compact {
                    let rendered = display::render_tool_output_with_cols(lines, cols);
                    for line in &rendered {
                        write_stdout(line);
                        write_stdout("\r\n");
                        count += Self::visual_rows(line, cols);
                    }
                }
            }
        }

        self.lines_printed = start_line + count;
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
                ScrollEntry::LlmText(text) => {
                    let mut out = vec![String::new()];
                    for (i, line) in text.split('\n').enumerate() {
                        if i == 0 {
                            out.push(format!("\x1b[97m●\x1b[0m {}", line));
                        } else {
                            out.push(format!("  {}", line));
                        }
                    }
                    out
                }
                ScrollEntry::Response(content) => {
                    let rendered = super::markdown::render(content);
                    let mut out = vec![String::new()]; // empty line before response
                    for (i, line) in rendered.split("\r\n").enumerate() {
                        if i == 0 {
                            out.push(format!("\x1b[97m●\x1b[0m {}", line));
                        } else {
                            out.push(format!("  {}", line));
                        }
                    }
                    out
                }
                ScrollEntry::Separator => {
                    vec![display::render_separator_plain(cols)]
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
        onboarded: &AtomicBool,
        cursor_col: u16,
        cursor_row: u16,
    ) -> ChatExitAction {
        // Eagerly update cwd so the daemon has the current value before any chat message.
        // Without this, polling lag (up to 60s) can cause chat to see a stale cwd (#354).
        if let Some(cwd) = crate::get_shell_cwd(proxy.child_pid() as u32) {
            self.shell_cwd = Some(cwd.clone());
            let mut attrs = std::collections::HashMap::new();
            attrs.insert("shell_cwd".to_string(), cwd);
            let msg = Message::SessionUpdate(SessionUpdate {
                session_id: session_id.to_string(),
                timestamp_ms: crate::timestamp_ms(),
                attrs,
            });
            // Use call() (not send()) to wait for Ack — the daemon spawns each
            // message as a separate tokio task, so fire-and-forget send() can race
            // with the subsequent ChatMessage.
            let _ = rpc.call(msg).await;
        }

        let is_resumed = initial_msg.as_ref()
            .map(|m| m.starts_with("/resume"))
            .unwrap_or(false);
        let show_ghost_hint = initial_msg.is_none() || is_resumed;
        self.pending_input = initial_msg;

        // Move past shell prompt to a new line
        write_stdout("\r\n");

        let mut exit_action = ChatExitAction::Normal;
        loop {
            let (input, is_fast_resume) = if let Some(msg) = self.pending_input.take() {
                (msg, true)
            } else {
                write_stdout("\x1b[36m> \x1b[0m");
                // Show ghost hint on first prompt
                if show_ghost_hint && !self.ghost_hint_shown {
                    self.ghost_hint_shown = true;
                    let hint = if let Some(ref model) = self.resumed_model {
                        format!("model for conversation: {}", model)
                    } else if is_resumed {
                        String::new()
                    } else {
                        "type to start, /resume to continue".to_string()
                    };
                    if !hint.is_empty() {
                        write_stdout(&format!("\x1b7\x1b[2;90m{}\x1b[0m\x1b8", hint));
                    }
                }
                crate::event_log::push(format!("chat_loop: entering read_input allow_backspace_exit={}", !self.has_activity));
                let result = self.read_input(!self.has_activity);
                crate::event_log::push(format!("chat_loop: read_input returned {}", if result.is_some() { "Some" } else { "None" }));
                match result {
                    Some(line) => {
                        write_stdout("\r\n");
                        (line, false)
                    }
                    None => break,
                }
            };

            let trimmed = input.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Mark onboarded on first chat entry
            if !onboarded.load(Ordering::Relaxed) {
                onboarded.store(true, Ordering::Relaxed);
                crate::onboarding::mark_onboarded();
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

            // /resume_tid <thread_id> (internal — used by :: resume shortcut)
            if let Some(tid) = trimmed.strip_prefix("/resume_tid ") {
                if !self.handle_resume_tid(tid.trim(), session_id, rpc).await && is_fast_resume {
                    break; // cancelled or failed on auto-resume — exit chat mode
                }
                continue;
            }

            // /resume [N]
            if trimmed == "/resume" || trimmed.starts_with("/resume ") {
                if !self.handle_resume(trimmed, session_id, rpc).await && is_fast_resume {
                    break; // cancelled or failed on auto-resume — exit chat mode
                }
                continue;
            }

            // /model
            if trimmed == "/model" {
                self.handle_model(session_id, rpc).await;
                continue;
            }

            // /test — hidden test commands
            if trimmed == "/test" || trimmed.starts_with("/test ") {
                let arg = trimmed.strip_prefix("/test").unwrap().trim();
                match arg {
                    "" => {
                        write_stdout("\x1b[2;90mAvailable /test commands:\x1b[0m\r\n");
                        write_stdout("\x1b[2;90m  /test picker [N]          — flat picker (N = initial index)\x1b[0m\r\n");
                        write_stdout("\x1b[2;90m  /test multi_level_picker  — cascading picker (3 levels)\x1b[0m\r\n");
                        write_stdout("\x1b[2;90m  /test menu                — multi-level menu widget\x1b[0m\r\n");
                    }
                    "multi_level_picker" => self.handle_test_multi_level_picker(),
                    "menu" => self.handle_test_menu(),
                    other => {
                        if other == "picker" || other.starts_with("picker ") {
                            let idx: usize = other.strip_prefix("picker")
                                .unwrap().trim().parse().unwrap_or(0);
                            self.handle_test_picker(idx);
                        } else {
                            write_stdout(&format!(
                                "\x1b[2;90mUnknown test: {}. Run /test for a list.\x1b[0m\r\n",
                                other
                            ));
                        }
                    }
                }
                continue;
            }

            // /config
            if trimmed == "/config" {
                self.handle_config(session_id, rpc).await;
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
                            super::handle_command_result(&display_text, Some(path), self.shell_cwd.as_deref());
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

            // /lock on|off
            if trimmed == "/lock on" || trimmed == "/lock off" {
                let lock = trimmed == "/lock on";
                exit_action = ChatExitAction::Lock(lock);
                break;
            }

            // /update auto
            let (without_redirect, redirect) = command::parse_redirect_pub(trimmed);
            let (base_cmd, limit) = command::parse_limit_pub(without_redirect);
            if base_cmd == "/update auto" {
                let prev = auto_update_enabled.load(Ordering::Relaxed);
                let new_val = !prev;
                auto_update_enabled.store(new_val, Ordering::Relaxed);
                // Persist to client.toml
                let config_path = std::env::var("OMNISH_CLIENT_CONFIG")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"));
                if let Err(e) = omnish_common::config_edit::set_toml_value(&config_path, "auto_update", new_val) {
                    tracing::warn!("cannot persist auto_update to client.toml: {}", e);
                }
                let status = if new_val { "enabled" } else { "disabled" };
                let result = format!("Auto-update {}", status);
                let display_result = if let Some(ref l) = limit {
                    command::apply_limit(&result, l)
                } else {
                    result
                };
                if let Some(path) = redirect {
                    super::handle_command_result(&display_result, Some(path), self.shell_cwd.as_deref());
                } else {
                    write_stdout(&display::render_response(&display_result));
                }
                if auto_exit { break; }
                continue;
            }

            // Other /commands
            if trimmed.starts_with('/')
                && super::handle_slash_command(
                    trimmed, session_id, rpc, proxy, self.shell_cwd.as_deref(), client_debug_fn, cursor_col, cursor_row,
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
                    thread_id: None,
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
                model: self.pending_model.take(),
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
                                                    self.print_line("");
                                                    for (i, line) in cts.status.split('\n').enumerate() {
                                                        if i == 0 {
                                                            self.print_line(&format!("\x1b[97m●\x1b[0m {}", line));
                                                        } else {
                                                            self.print_line(&format!("  {}", line));
                                                        }
                                                    }
                                                    self.push_entry(ScrollEntry::LlmText(cts.status.clone()));
                                                } else if cts.result_compact.is_none() {
                                                    // First status — tool is running (before execution)
                                                    if self.tool_section_start.is_none() {
                                                        self.tool_section_start = Some(self.lines_printed);
                                                        self.tool_section_hist_idx = Some(self.scroll_history.len());
                                                    }
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
                                                    // Re-render entire tool section with updated statuses
                                                    self.redraw_tool_section();
                                                }
                                            }
                                            Some(Message::ChatToolCall(tc)) => {
                                                tool_calls.push(tc);
                                            }
                                            Some(Message::ChatResponse(resp)) if resp.request_id == req_id => {
                                                self.erase_thinking();
                                                self.tool_section_start = None;
                                                self.tool_section_hist_idx = None;
                                                self.print_line("");
                                                let rendered = markdown::render(&resp.content);
                                                for (i, line) in rendered.split("\r\n").enumerate() {
                                                    if i == 0 {
                                                        self.print_line(&format!("\x1b[97m●\x1b[0m {}", line));
                                                    } else {
                                                        self.print_line(&format!("  {}", line));
                                                    }
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

                            // Phase 2+3: Execute tools in parallel, send results as they complete
                            let shell_cwd = super::get_shell_cwd(proxy.child_pid() as u32);
                            let total = tool_calls.len();
                            let mut join_set = tokio::task::JoinSet::new();
                            for (idx, tc) in tool_calls.iter().enumerate() {
                                let plugins = Arc::clone(&self.client_plugins);
                                let tool_name = tc.tool_name.clone();
                                let plugin_name = tc.plugin_name.clone();
                                let sandboxed = tc.sandboxed;
                                if !sandboxed {
                                    crate::event_log::push(format!(
                                        "tool '{}' running without sandbox (permit rule match)",
                                        tc.tool_name,
                                    ));
                                }
                                let tool_input: serde_json::Value =
                                    serde_json::from_str(&tc.input).unwrap_or_default();
                                let cwd = shell_cwd.clone();
                                join_set.spawn(async move {
                                    let result = tokio::task::spawn_blocking(move || {
                                        plugins.execute_tool(
                                            &plugin_name,
                                            &tool_name,
                                            &tool_input,
                                            cwd.as_deref(),
                                            sandboxed,
                                        )
                                    }).await;
                                    (idx, result)
                                });
                            }

                            let mut completed = 0;
                            let mut send_failed = false;
                            loop {
                                let mut crx2 = cancel_rx.clone();
                                tokio::select! {
                                    next = join_set.join_next() => {
                                        match next {
                                            Some(Ok((idx, result))) => {
                                                completed += 1;
                                                let tc = &tool_calls[idx];
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

                                                if completed < total {
                                                    // Intermediate result — send and render status inline
                                                    match rpc.call(result_msg).await {
                                                        Ok(Message::ChatToolStatus(cts)) => {
                                                            // Update running header in-place: move cursor up to the tool's line
                                                            let lines_up = total - idx;
                                                            let (_, cols) = super::get_terminal_size().unwrap_or((24, 80));
                                                            let display_name = cts.display_name.as_deref().unwrap_or(&cts.tool_name);
                                                            let param_desc = cts.param_desc.as_deref().unwrap_or("");
                                                            let icon = cts.status_icon.as_ref().unwrap_or(&StatusIcon::Success);
                                                            let header = display::render_tool_header(icon, display_name, param_desc, cols as usize);
                                                            write_stdout(&format!("\x1b[{}A\r\x1b[K{}\x1b[{}B\r", lines_up, header, lines_up));
                                                            // Update scroll_history entry
                                                            let tool_call_id = cts.tool_call_id.clone();
                                                            if let Some(entry) = self.scroll_history.iter_mut().rev().find(|e| {
                                                                matches!(e, ScrollEntry::ToolStatus(prev) if prev.tool_call_id == tool_call_id)
                                                            }) {
                                                                *entry = ScrollEntry::ToolStatus(cts);
                                                            }
                                                        }
                                                        Err(e) => {
                                                            write_stdout(&display::render_error(&format!(
                                                                "Failed to send tool result: {}",
                                                                e
                                                            )));
                                                            send_failed = true;
                                                            break;
                                                        }
                                                        _ => {} // Ack or other
                                                    }
                                                } else {
                                                    // Last result — switch to streaming for agent loop continuation
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
                                            Some(Err(_)) => {
                                                // JoinSet task panicked
                                                completed += 1;
                                                if completed >= total { break; }
                                            }
                                            None => break, // All tasks done
                                        }
                                    }
                                    _ = wait_cancel(&mut crx2) => {
                                        interrupted = true;
                                        break 'stream;
                                    }
                                }
                            }
                            if send_failed {
                                break 'stream;
                            }

                            // Sync shell_cwd after tools execute — tools like glob/read may
                            // change cwd via picker interaction, and we need the updated cwd
                            // for the next round of tool calls.
                            if let Some(cwd) = super::get_shell_cwd(proxy.child_pid() as u32) {
                                self.shell_cwd = Some(cwd.clone());
                                let mut attrs = std::collections::HashMap::new();
                                attrs.insert("shell_cwd".to_string(), cwd);
                                let msg = Message::SessionUpdate(SessionUpdate {
                                    session_id: session_id.to_string(),
                                    timestamp_ms: crate::timestamp_ms(),
                                    attrs,
                                });
                                let _ = rpc.call(msg).await;
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
                self.print_line("");
                self.print_line("\x1b[97m●\x1b[0m User interrupted. What should I do instead?");
                self.push_entry(ScrollEntry::Response("User interrupted. What should I do instead?".to_string()));

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

        // Release the thread binding on the daemon so other sessions can use it
        if let Some(ref tid) = self.current_thread_id {
            let msg = Message::ChatEnd(ChatEnd {
                session_id: session_id.to_string(),
                thread_id: tid.clone(),
            });
            let _ = rpc.call(msg).await;
        }
        exit_action
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

    async fn handle_resume(&mut self, trimmed: &str, session_id: &str, rpc: &RpcClient) -> bool {
        // Resolve which thread_id to resume, then delegate to handle_resume_tid
        let tid: Option<String> = if let Some(idx_str) = trimmed.strip_prefix("/resume ") {
            // Auto-fetch if cache empty
            if self.cached_thread_ids.is_empty() {
                self.fetch_thread_ids(session_id, rpc).await;
            }
            match idx_str.trim().parse::<usize>() {
                Ok(i) if i >= 1 && i <= self.cached_thread_ids.len() => {
                    let t = self.cached_thread_ids[i - 1].clone();
                    if t.is_empty() {
                        write_stdout(&display::render_error(&format!(
                            "Conversation [{}] was deleted", i
                        )));
                        None
                    } else {
                        Some(t)
                    }
                }
                Ok(i) if i >= 1 => {
                    if self.cached_thread_ids.is_empty() {
                        write_stdout(&display::render_error("No conversation to resume"));
                    } else {
                        write_stdout(&display::render_error(&format!(
                            "Index {} out of range ({} conversations)",
                            i, self.cached_thread_ids.len()
                        )));
                    }
                    None
                }
                _ => {
                    write_stdout(&display::render_error("Invalid index"));
                    None
                }
            }
        } else {
            // /resume without index — picker (with lock-aware disabled items)
            self.show_resume_picker(session_id, rpc).await
        };

        if let Some(tid) = tid {
            self.handle_resume_tid(&tid, session_id, rpc).await
        } else {
            false
        }
    }

    /// Fetch and cache thread IDs from the daemon.
    async fn fetch_thread_ids(&mut self, session_id: &str, rpc: &RpcClient) {
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

    /// Show the resume picker with lock-aware disabled items.
    /// Returns the selected thread_id, or None on ESC/cancel.
    async fn show_resume_picker(&mut self, session_id: &str, rpc: &RpcClient) -> Option<String> {
        self.fetch_thread_ids(session_id, rpc).await;
        if self.cached_thread_ids.is_empty() {
            write_stdout(&display::render_error("No conversations to resume"));
            return None;
        }
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
                        return None;
                    }
                    // Build disabled flags from locked_threads
                    use widgets::picker::DisabledIcon;
                    let disabled: Vec<Option<DisabledIcon>> = json.get("locked_threads")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().map(|v| {
                            if v.as_bool().unwrap_or(false) { Some(DisabledIcon::Key) } else { None }
                        }).collect())
                        .unwrap_or_else(|| vec![None; items.len()]);
                    match widgets::picker::pick_one_with_disabled("Resume conversation:", &items, &disabled) {
                        Some(idx) if idx < self.cached_thread_ids.len() => {
                            Some(self.cached_thread_ids[idx].clone())
                        }
                        _ => None,
                    }
                } else {
                    write_stdout(&display::render_error("No conversations to resume"));
                    None
                }
            }
            _ => {
                write_stdout(&display::render_error("Failed to list conversations"));
                None
            }
        }
    }

    /// Apply a ChatReady response: handle errors, set thread, render history.
    fn apply_chat_ready(&mut self, ready: ChatReady) {
        // Error from daemon (thread_locked, not_found, etc.)
        if let Some(ref err_display) = ready.error_display {
            write_stdout(&display::render_error(err_display));
            return;
        }
        if ready.error.is_some() {
            write_stdout(&display::render_error("Failed to resume conversation"));
            return;
        }
        if ready.thread_id.is_empty() {
            write_stdout(&display::render_error("Failed to resume conversation"));
            return;
        }

        self.current_thread_id = Some(ready.thread_id);

        if let Some(history) = ready.history {
            // Parse structured history entries (each is a JSON-encoded string)
            let mut all_entries: Vec<ScrollEntry> = Vec::new();
            for entry_str in &history {
                let entry: serde_json::Value = match serde_json::from_str(entry_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
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
                            let rendered = display::render_tool_output_with_cols(lines, cols as usize);
                            for line in &rendered {
                                self.print_line(line);
                            }
                        }
                    }
                    ScrollEntry::LlmText(text) => {
                        self.print_line("");
                        for (i, line) in text.split('\n').enumerate() {
                            if i == 0 {
                                self.print_line(&format!("\x1b[97m●\x1b[0m {}", line));
                            } else {
                                self.print_line(&format!("  {}", line));
                            }
                        }
                    }
                    ScrollEntry::Response(content) => {
                        self.print_line("");
                        let rendered = markdown::render(content);
                        for (i, line) in rendered.split("\r\n").enumerate() {
                            if i == 0 {
                                self.print_line(&format!("\x1b[97m●\x1b[0m {}", line));
                            } else {
                                self.print_line(&format!("  {}", line));
                            }
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

        // Store non-default model name for ghost hint
        if let Some(model) = ready.model_name {
            self.resumed_model = Some(model);
        }
    }

    /// Resume a specific thread by ID via ChatStart protocol message.
    /// Returns `true` if the thread was successfully resumed, `false` if cancelled or failed.
    async fn handle_resume_tid(&mut self, tid: &str, session_id: &str, rpc: &RpcClient) -> bool {
        crate::event_log::push(format!("resume_tid: sending ChatStart thread={}", tid));
        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let start_msg = Message::ChatStart(ChatStart {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            new_thread: false,
            thread_id: Some(tid.to_string()),
        });
        crate::event_log::push("resume_tid: awaiting ChatReady (timeout 15s)");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            rpc.call(start_msg),
        ).await;
        match result {
            Ok(Ok(Message::ChatReady(ready))) if ready.request_id == rid => {
                crate::event_log::push(format!("resume_tid: got ChatReady error={:?}", ready.error));

                // If thread is locked, show picker to let user choose another thread
                if ready.error.as_deref() == Some("thread_locked") {
                    crate::event_log::push("resume_tid: thread locked, showing picker");
                    if let Some(alt_tid) = self.show_resume_picker(session_id, rpc).await {
                        // Resume the selected thread (locked items are disabled in picker,
                        // so this should not hit thread_locked again)
                        let rid2 = Uuid::new_v4().to_string()[..8].to_string();
                        let start2 = Message::ChatStart(ChatStart {
                            request_id: rid2.clone(),
                            session_id: session_id.to_string(),
                            new_thread: false,
                            thread_id: Some(alt_tid),
                        });
                        if let Ok(Ok(Message::ChatReady(r2))) = tokio::time::timeout(
                            std::time::Duration::from_secs(15),
                            rpc.call(start2),
                        ).await {
                            if r2.request_id == rid2 {
                                self.apply_chat_ready(r2);
                                return true;
                            }
                        }
                        write_stdout(&display::render_error("Failed to resume conversation"));
                    }
                    return false;
                }

                // Render history first, then check mismatch
                self.apply_chat_ready(ready.clone());
                crate::event_log::push("resume_tid: apply_chat_ready done");

                if ready.error.is_none() && !ready.thread_id.is_empty() {
                    if let Some(action) = self.check_resume_mismatch(&ready) {
                        match action {
                            ResumeMismatchAction::Cancel => {
                                crate::event_log::push("resume_tid: user cancelled due to cwd/host mismatch");
                                write_stdout("\x1b[2;37m(User canceled)\x1b[0m\r\n");
                                // Release the thread claim
                                let end_msg = Message::ChatEnd(ChatEnd {
                                    session_id: session_id.to_string(),
                                    thread_id: ready.thread_id.clone(),
                                });
                                let _ = rpc.send(end_msg).await;
                                return false;
                            }
                            ResumeMismatchAction::CdToOld(old_cwd) => {
                                // Update shell_cwd so daemon uses correct cwd for bash tools
                                self.shell_cwd = Some(old_cwd.clone());
                                let mut attrs = std::collections::HashMap::new();
                                attrs.insert("shell_cwd".to_string(), old_cwd.clone());
                                let msg = Message::SessionUpdate(SessionUpdate {
                                    session_id: session_id.to_string(),
                                    timestamp_ms: crate::timestamp_ms(),
                                    attrs,
                                });
                                let _ = rpc.send(msg).await;
                                self.pending_cd = Some(old_cwd.clone());
                                write_stdout(&format!("\x1b[2;37mcwd changed: {}\x1b[0m\r\n", old_cwd));
                            }
                            ResumeMismatchAction::StayHere(_old_cwd) => {}
                            ResumeMismatchAction::ContinueDifferentHost => {}
                        }
                    }
                }
                crate::event_log::push("resume_tid: done");
                return true;
            }
            Ok(Ok(msg)) => {
                crate::event_log::push(format!("resume_tid: unexpected response {:?}", std::mem::discriminant(&msg)));
                write_stdout(&display::render_error("Failed to resume conversation"));
            }
            Ok(Err(e)) => {
                crate::event_log::push(format!("resume_tid: RPC error: {}", e));
                write_stdout(&display::render_error("Failed to resume conversation"));
            }
            Err(_) => {
                let connected = rpc.is_connected().await;
                crate::event_log::push(format!("resume_tid: timed out waiting for daemon response (connected={})", connected));
                write_stdout(&display::render_error("Resume timed out — daemon may be busy"));
            }
        }
        crate::event_log::push("resume_tid: done");
        false
    }

    // ── Resume mismatch check ─────────────────────────────────────────────

    /// Compare thread's previous host/cwd against current environment.
    /// Returns None if no mismatch, or Some(action) after prompting the user.
    fn check_resume_mismatch(&self, ready: &ChatReady) -> Option<ResumeMismatchAction> {
        let cur_host = nix::unistd::gethostname()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_default();
        let cur_cwd = self.shell_cwd.clone().unwrap_or_default();

        let thread_host = ready.thread_host.as_deref().unwrap_or("");
        let thread_cwd = ready.thread_cwd.as_deref().unwrap_or("");

        // No previous data — nothing to compare
        if thread_host.is_empty() && thread_cwd.is_empty() {
            return None;
        }

        let same_host = thread_host.is_empty() || thread_host == cur_host;
        let same_cwd = thread_cwd.is_empty() || thread_cwd == cur_cwd;

        if same_host && same_cwd {
            return None;
        }

        if !same_host {
            // Different machine
            let title = format!(
                "This conversation was on \x1b[36m{}\x1b[0m (current: \x1b[36m{}\x1b[0m). Proceed?",
                thread_host, cur_host,
            );
            let items = &["[Y]es", "[C]ancel"];
            match widgets::picker::pick_one(&title, items) {
                Some(0) => Some(ResumeMismatchAction::ContinueDifferentHost),
                _ => Some(ResumeMismatchAction::Cancel),
            }
        } else {
            // Same machine, different cwd
            let title = format!(
                "Switch to \x1b[34m{}\x1b[0m (last conversation path)?",
                thread_cwd,
            );
            let items = &["[Y]es", "[N]o, stay here", "[C]ancel"];
            match widgets::picker::pick_one(&title, items) {
                Some(0) => Some(ResumeMismatchAction::CdToOld(thread_cwd.to_string())),
                Some(1) => Some(ResumeMismatchAction::StayHere(thread_cwd.to_string())),
                _ => Some(ResumeMismatchAction::Cancel),
            }
        }
    }

    // ── Model picker ─────────────────────────────────────────────────────

    async fn handle_model(&mut self, session_id: &str, rpc: &RpcClient) {
        // Build query with thread_id if available
        let query = match &self.current_thread_id {
            Some(tid) => format!("__cmd:models {}", tid),
            None => "__cmd:models".to_string(),
        };

        let rid = Uuid::new_v4().to_string()[..8].to_string();
        let req = Message::Request(Request {
            request_id: rid.clone(),
            session_id: session_id.to_string(),
            query,
            scope: RequestScope::AllSessions,
        });

        let models = match rpc.call(req).await {
            Ok(Message::Response(resp)) if resp.request_id == rid => {
                match super::parse_cmd_response(&resp.content) {
                    Some(json) => json.get("models").and_then(|v| v.as_array()).cloned(),
                    None => None,
                }
            }
            _ => None,
        };

        let models = match models {
            Some(m) if !m.is_empty() => m,
            _ => {
                write_stdout(&display::render_error("No LLM backends available"));
                return;
            }
        };

        // Build picker items and find selected index
        let mut selected_idx = 0;
        let item_strings: Vec<String> = models.iter().enumerate().map(|(i, m)| {
            let name = m["name"].as_str().unwrap_or("?");
            let model = m["model"].as_str().unwrap_or("?");
            let short_model = strip_date_suffix(model);
            if m["selected"].as_bool().unwrap_or(false) {
                selected_idx = i;
            }
            format!("{} ({})", name, short_model)
        }).collect();
        let items: Vec<&str> = item_strings.iter().map(|s| s.as_str()).collect();

        match widgets::picker::pick_one_at("Select model:", &items, selected_idx) {
            Some(idx) if idx < models.len() => {
                let name = models[idx]["name"].as_str().unwrap_or("").to_string();
                let display_name = &item_strings[idx];

                if let Some(ref tid) = self.current_thread_id {
                    // Existing thread — send model-only ChatMessage
                    let rid = Uuid::new_v4().to_string()[..8].to_string();
                    let msg = Message::ChatMessage(omnish_protocol::message::ChatMessage {
                        request_id: rid.clone(),
                        session_id: session_id.to_string(),
                        thread_id: tid.clone(),
                        query: String::new(),
                        model: Some(name),
                    });
                    match rpc.call(msg).await {
                        Ok(Message::Ack) => {
                            write_stdout(&format!("\x1b[2;90mSwitched to {}\x1b[0m\r\n", display_name));
                        }
                        _ => {
                            write_stdout(&display::render_error("Failed to switch model"));
                        }
                    }
                } else {
                    // New thread — defer model selection to first message
                    self.pending_model = Some(name);
                    write_stdout(&format!("\x1b[2;90mSwitched to {}\x1b[0m\r\n", display_name));
                }
            }
            _ => {} // ESC or no selection — do nothing
        }
    }

    // ── Test helpers (hidden from /help) ────────────────────────────────

    fn handle_test_picker(&self, selected_idx: usize) {
        let items: Vec<String> = (1..=20)
            .map(|i| format!("test-backend-{} (test-model-{})", i, i))
            .collect();
        let refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
        let idx = selected_idx.min(items.len().saturating_sub(1));
        let result = widgets::picker::pick_one_at("Select model:", &refs, idx);
        let msg = match result {
            Some(idx) => format!("Selected: {}", items[idx]),
            None => "Cancelled".to_string(),
        };
        write_stdout(&format!("\x1b[2;90m{}\x1b[0m\r\n", msg));
    }

    fn handle_test_menu(&self) {
        use widgets::menu::{MenuItem, MenuResult};

        let mut items = vec![
            MenuItem::Submenu {
                label: "LLM".to_string(),
                children: vec![
                    MenuItem::Select {
                        label: "Default backend".to_string(),
                        options: vec![
                            "claude".to_string(),
                            "openai".to_string(),
                            "local".to_string(),
                        ],
                        selected: 0,
                    },
                    MenuItem::Toggle {
                        label: "Streaming".to_string(),
                        value: true,
                    },
                    MenuItem::TextInput {
                        label: "API key".to_string(),
                        value: "sk-***".to_string(),
                    },
                    MenuItem::TextInput {
                        label: "Proxy URL".to_string(),
                        value: String::new(),
                    },
                ],
                handler: None,
            },
            MenuItem::Submenu {
                label: "Shell".to_string(),
                children: vec![
                    MenuItem::Toggle {
                        label: "Developer mode".to_string(),
                        value: false,
                    },
                    MenuItem::Toggle {
                        label: "Completion enabled".to_string(),
                        value: true,
                    },
                    MenuItem::Select {
                        label: "Theme".to_string(),
                        options: vec![
                            "default".to_string(),
                            "minimal".to_string(),
                            "compact".to_string(),
                        ],
                        selected: 0,
                    },
                ],
                handler: None,
            },
            MenuItem::Toggle {
                label: "Telemetry".to_string(),
                value: false,
            },
            MenuItem::TextInput {
                label: "Username".to_string(),
                value: "user".to_string(),
            },
        ];

        let result = widgets::menu::run_menu("Config", &mut items, None);
        match result {
            MenuResult::Done(changes) => {
                if changes.is_empty() {
                    write_stdout("\x1b[2;90mNo changes made.\x1b[0m\r\n");
                } else {
                    write_stdout(&format!(
                        "\x1b[2;90mChanges ({}):\x1b[0m\r\n",
                        changes.len()
                    ));
                    for c in &changes {
                        write_stdout(&format!(
                            "\x1b[2;90m  {} = {}\x1b[0m\r\n",
                            c.path, c.value
                        ));
                    }
                }
            }
            MenuResult::Cancelled => {
                write_stdout("\x1b[2;90mCancelled.\x1b[0m\r\n");
            }
        }
    }

    async fn handle_config(&mut self, _session_id: &str, rpc: &RpcClient) {
        let (items, handlers) = match rpc.call(Message::ConfigQuery).await {
            Ok(Message::ConfigResponse { items, handlers }) => (items, handlers),
            Ok(_) => {
                write_stdout("\x1b[31mUnexpected response from daemon\x1b[0m\r\n");
                return;
            }
            Err(e) => {
                write_stdout(&format!("\x1b[31mFailed to query config: {}\x1b[0m\r\n", e));
                return;
            }
        };

        if items.is_empty() {
            write_stdout("\x1b[2;90mNo configurable items.\x1b[0m\r\n");
            return;
        }

        let (mut menu_items, path_map_initial) = build_menu_tree(&items, &handlers);
        let path_map = RefCell::new(path_map_initial);

        let rpc_ref = rpc;
        let path_map_ref = &path_map;

        let result = tokio::task::block_in_place(|| {
            let rt = tokio::runtime::Handle::current();

            let mut handler_callback = |_handler_name: &str, handler_changes: Vec<widgets::menu::MenuChange>| -> Option<Vec<widgets::menu::MenuItem>> {
                let pm = path_map_ref.borrow();
                let config_changes: Vec<ConfigChange> = handler_changes.iter()
                    .map(|mc| {
                        let schema_path = pm.get(&mc.path)
                            .cloned()
                            .unwrap_or_else(|| mc.path.clone());
                        ConfigChange { path: schema_path, value: mc.value.clone() }
                    })
                    .collect();
                drop(pm);

                let update_result = rt.block_on(async {
                    rpc_ref.call(Message::ConfigUpdate { changes: config_changes }).await
                });

                match update_result {
                    Ok(Message::ConfigUpdateResult { ok: true, .. }) => {
                        let query_result = rt.block_on(async {
                            rpc_ref.call(Message::ConfigQuery).await
                        });
                        match query_result {
                            Ok(Message::ConfigResponse { items, handlers: new_handlers }) => {
                                let (new_tree, new_map) = build_menu_tree(&items, &new_handlers);
                                *path_map_ref.borrow_mut() = new_map;
                                Some(new_tree)
                            }
                            _ => None,
                        }
                    }
                    Ok(Message::ConfigUpdateResult { ok: false, error }) => {
                        write_stdout(&format!("\x1b[31mHandler error: {}\x1b[0m\r\n",
                            error.unwrap_or_default()));
                        None
                    }
                    _ => None,
                }
            };

            widgets::menu::run_menu("Config", &mut menu_items, Some(&mut handler_callback))
        });

        match result {
            widgets::menu::MenuResult::Done(changes) => {
                if changes.is_empty() {
                    write_stdout("\x1b[2;90mNo changes made.\x1b[0m\r\n");
                    return;
                }
                let pm = path_map.borrow();
                let config_changes: Vec<ConfigChange> = changes.iter()
                    .map(|mc| {
                        let schema_path = pm.get(&mc.path)
                            .cloned()
                            .unwrap_or_else(|| mc.path.clone());
                        ConfigChange { path: schema_path, value: mc.value.clone() }
                    })
                    .collect();
                drop(pm);

                match rpc.call(Message::ConfigUpdate { changes: config_changes }).await {
                    Ok(Message::ConfigUpdateResult { ok: true, .. }) => {
                        write_stdout(&format!(
                            "\x1b[2;90mConfig saved ({}). Restart daemon to apply.\x1b[0m\r\n",
                            changes.len()
                        ));
                    }
                    Ok(Message::ConfigUpdateResult { ok: false, error }) => {
                        write_stdout(&format!("\x1b[31mFailed to save: {}\x1b[0m\r\n",
                            error.unwrap_or_default()));
                    }
                    Err(e) => {
                        write_stdout(&format!("\x1b[31mRPC error: {}\x1b[0m\r\n", e));
                    }
                    _ => {}
                }
            }
            widgets::menu::MenuResult::Cancelled => {
                write_stdout("\x1b[2;90mCancelled.\x1b[0m\r\n");
            }
        }
    }

    fn handle_test_multi_level_picker(&self) {
        // Level 1: category selection
        let categories = &["Fruits", "Vegetables", "Drinks"];
        let cat_idx = match widgets::picker::pick_one("Select category:", categories) {
            Some(idx) => idx,
            None => {
                write_stdout("\x1b[2;90mCancelled at level 1\x1b[0m\r\n");
                return;
            }
        };
        write_stdout(&format!(
            "\x1b[2;90mCategory: {}\x1b[0m\r\n",
            categories[cat_idx]
        ));

        // Level 2: item selection within category
        let items: &[&[&str]] = &[
            &["Apple", "Banana", "Cherry", "Durian"],
            &["Carrot", "Broccoli", "Spinach"],
            &["Water", "Coffee", "Tea", "Juice", "Milk"],
        ];
        let title = format!("Select {} item:", categories[cat_idx].to_lowercase());
        let item_idx = match widgets::picker::pick_one(&title, items[cat_idx]) {
            Some(idx) => idx,
            None => {
                write_stdout("\x1b[2;90mCancelled at level 2\x1b[0m\r\n");
                return;
            }
        };
        let selected = items[cat_idx][item_idx];
        write_stdout(&format!(
            "\x1b[2;90mItem: {}\x1b[0m\r\n",
            selected
        ));

        // Level 3: action selection
        let actions = &["[A]dd to cart", "[V]iew details", "[C]ancel"];
        let action_idx = match widgets::picker::pick_one("Action:", actions) {
            Some(idx) => idx,
            None => {
                write_stdout("\x1b[2;90mCancelled at level 3\x1b[0m\r\n");
                return;
            }
        };

        let result = format!(
            "Result: {} > {} > {}",
            categories[cat_idx], selected, actions[action_idx]
        );
        write_stdout(&format!("\x1b[2;90m{}\x1b[0m\r\n", result));
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

            // Idle timeout: exit chat after 30 minutes of no input
            {
                let mut pfd = libc::pollfd { fd: stdin_fd, events: libc::POLLIN, revents: 0 };
                let timeout_ms = 30 * 60 * 1000; // 30 minutes
                let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
                if ready == 0 {
                    // Timeout — auto-exit chat mode
                    write_stdout("\r\n\x1b[2;37m(chat closed due to inactivity)\x1b[0m\r\n");
                    // Disable bracketed paste before exiting
                    write_stdout("\x1b[?2004l");
                    return None;
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
