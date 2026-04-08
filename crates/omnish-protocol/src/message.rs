use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAGIC: [u8; 2] = [0x4F, 0x53]; // "OS" for OmniSh

/// Protocol version — increment on incompatible wire format changes.
pub const PROTOCOL_VERSION: u32 = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigItem {
    pub path: String,
    pub label: String,
    pub kind: ConfigItemKind,
    /// For Select items in forms: maps each option to sibling field values to prefill.
    /// `Vec<(option_name, Vec<(sibling_label, value)>)>`
    #[serde(default)]
    pub prefills: Vec<(String, Vec<(String, String)>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigItemKind {
    Toggle { value: bool },
    Select { options: Vec<String>, selected: usize },
    TextInput { value: String },
    /// Non-interactive label for displaying descriptions or section notes.
    Label,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChange {
    pub path: String,
    pub value: String,
}

/// Metadata about handler submenus — sent alongside items so the client
/// knows which submenus trigger handler callbacks and what label to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigHandlerInfo {
    /// Schema path of the handler submenu (e.g., "llm.backends.__new__").
    pub path: String,
    /// Display label (e.g., "Add backend"). Used for `__new__` segments
    /// that can't be auto-labeled from the path.
    pub label: String,
    /// Handler function name (e.g., "add_backend").
    pub handler: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    SessionUpdate(SessionUpdate),
    IoData(IoData),
    Event(Event),
    Request(Request),
    Response(Response),
    CommandComplete(CommandComplete),
    CompletionRequest(CompletionRequest),
    CompletionResponse(CompletionResponse),
    CompletionSummary(CompletionSummary),
    ChatStart(ChatStart),
    ChatReady(ChatReady),
    ChatEnd(ChatEnd),
    ChatMessage(ChatMessage),
    ChatResponse(ChatResponse),
    ChatInterrupt(ChatInterrupt),
    ChatToolStatus(ChatToolStatus),
    ChatToolCall(ChatToolCall),
    ChatToolResult(ChatToolResult),
    Ack,
    Auth(Auth),
    AuthResult(AuthResult),
    ConfigQuery,
    ConfigResponse {
        items: Vec<ConfigItem>,
        handlers: Vec<ConfigHandlerInfo>,
    },
    ConfigUpdate { changes: Vec<ConfigChange> },
    ConfigUpdateResult { ok: bool, error: Option<String> },
    UpdateCheck {
        os: String,
        arch: String,
        current_version: String,
        hostname: String,
    },
    UpdateInfo {
        latest_version: String,
        checksum: String,
        available: bool,
    },
    UpdateRequest {
        os: String,
        arch: String,
        version: String,
        hostname: String,
    },
    UpdateChunk {
        seq: u32,
        total_size: u64,
        checksum: String,
        data: Vec<u8>,
        done: bool,
        error: Option<String>,
    },
    // New variants MUST be added at the end to preserve bincode variant indices.
    // Inserting in the middle shifts indices and breaks old clients.
    ConfigClient { changes: Vec<ConfigChange> },
    /// Test helper: daemon closes this connection after `delay_secs` seconds.
    TestDisconnect { delay_secs: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    pub token: String,
    #[serde(default)]
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    pub ok: bool,
    pub protocol_version: u32,
    #[serde(default)]
    pub daemon_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStart {
    pub session_id: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub attrs: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnd {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub session_id: String,
    pub timestamp_ms: u64,
    /// Attributes (includes host, shell_cwd, child_process from probes)
    pub attrs: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoData {
    pub session_id: String,
    pub direction: IoDirection,
    pub timestamp_ms: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub session_id: String,
    pub timestamp_ms: u64,
    pub event_type: EventType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    NonZeroExit(i32),
    PatternMatch(String),
    CommandBoundary { command: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub request_id: String,
    pub session_id: String,
    pub query: String,
    pub scope: RequestScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestScope {
    CurrentSession,
    AllSessions,
    Sessions(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub request_id: String,
    pub content: String,
    pub is_streaming: bool,
    pub is_final: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandComplete {
    pub session_id: String,
    pub record: omnish_store::command::CommandRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSuggestion {
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub session_id: String,
    pub input: String,
    pub cursor_pos: usize,
    pub sequence_id: u64,
    /// Current working directory at the time of request
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub sequence_id: u64,
    pub suggestions: Vec<CompletionSuggestion>,
}

/// Summary of a completion interaction for analytics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSummary {
    /// Session ID
    pub session_id: String,
    /// Sequence ID of the completion request
    pub sequence_id: u64,
    /// User input at the time of request
    pub prompt: String,
    /// The suggested completion text
    pub completion: String,
    /// Whether the user accepted the completion (Tab key)
    pub accepted: bool,
    /// Time from request to response (milliseconds)
    pub latency_ms: u64,
    /// Time from response to accept/ignore (milliseconds)
    pub dwell_time_ms: Option<u64>,
    /// Current working directory at the time of request
    pub cwd: Option<String>,
    /// Extra metadata as key-value pairs.
    /// Stored as `HashMap<String, String>` (not `Value`) because bincode cannot
    /// serialize/deserialize `serde_json::Value` (it calls `deserialize_any`).
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStart {
    pub request_id: String,
    pub session_id: String,
    pub new_thread: bool,
    /// If set, resume this specific thread instead of creating a new one.
    #[serde(default)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatReady {
    pub request_id: String,
    pub thread_id: String,
    pub last_exchange: Option<(String, String)>,
    pub earlier_count: u32,
    /// Model name from the chat LLM backend (e.g. "claude-sonnet-4-5-20250929").
    #[serde(default)]
    pub model_name: Option<String>,
    /// Structured conversation history (for resumed threads).
    /// Each entry is a JSON-encoded string (bincode cannot deserialize serde_json::Value directly).
    #[serde(default)]
    pub history: Option<Vec<String>>,
    /// Host where the thread was last used.
    #[serde(default)]
    pub thread_host: Option<String>,
    /// Working directory where the thread was last used.
    #[serde(default)]
    pub thread_cwd: Option<String>,
    /// Summary of the conversation thread.
    #[serde(default)]
    pub thread_summary: Option<String>,
    /// Error key when thread cannot be entered (e.g. "thread_locked").
    #[serde(default)]
    pub error: Option<String>,
    /// Human-readable error message.
    #[serde(default)]
    pub error_display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEnd {
    pub session_id: String,
    pub thread_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub request_id: String,
    pub session_id: String,
    pub thread_id: String,
    pub query: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub request_id: String,
    pub thread_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInterrupt {
    pub request_id: String,
    pub session_id: String,
    pub thread_id: String,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StatusIcon {
    Running,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolStatus {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub status: String,
    pub tool_call_id: Option<String>,
    pub status_icon: Option<StatusIcon>,
    pub display_name: Option<String>,
    pub param_desc: Option<String>,
    pub result_compact: Option<Vec<String>>,
    pub result_full: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolCall {
    pub request_id: String,
    pub thread_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    /// Tool input as JSON string (bincode cannot deserialize serde_json::Value)
    pub input: String,
    /// Plugin directory name ("builtin" or external plugin name)
    pub plugin_name: String,
    /// Whether to apply Landlock sandbox when spawning the plugin process
    pub sandboxed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatToolResult {
    pub request_id: String,
    pub thread_id: String,
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
    /// Tool requests LLM summarization of its result.
    #[serde(default)]
    pub needs_summarization: bool,
}

impl Message {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let payload = bincode::serialize(self)?;
        let len = payload.len() as u32;
        let mut buf = Vec::with_capacity(2 + 4 + payload.len());
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&payload);
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            bail!("message too short");
        }
        if bytes[0..2] != MAGIC {
            bail!("invalid magic bytes");
        }
        let len = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
        if bytes.len() < 6 + len {
            bail!("message truncated");
        }
        let msg: Message = bincode::deserialize(&bytes[6..6 + len])?;
        Ok(msg)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub request_id: u64,
    pub payload: Message,
}

impl Frame {
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let payload_bytes = self.payload.to_bytes()?;
        let mut buf = Vec::with_capacity(8 + payload_bytes.len());
        buf.extend_from_slice(&self.request_id.to_be_bytes());
        buf.extend_from_slice(&payload_bytes);
        Ok(buf)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 8 {
            bail!("frame too short");
        }
        let request_id = u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let payload = Message::from_bytes(&bytes[8..])?;
        Ok(Self { request_id, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_round_trip() {
        let frame = Frame {
            request_id: 42,
            payload: Message::Ack,
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 42);
        assert!(matches!(decoded.payload, Message::Ack));
    }

    #[test]
    fn test_frame_with_session_start() {
        let frame = Frame {
            request_id: 1,
            payload: Message::SessionStart(SessionStart {
                session_id: "abc".to_string(),
                parent_session_id: None,
                timestamp_ms: 1000,
                attrs: HashMap::new(),
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 1);
        assert!(matches!(decoded.payload, Message::SessionStart(_)));
    }

    #[test]
    fn test_frame_with_completion_request() {
        let frame = Frame {
            request_id: 10,
            payload: Message::CompletionRequest(CompletionRequest {
                session_id: "abc".to_string(),
                input: "git sta".to_string(),
                cursor_pos: 7,
                sequence_id: 42,
                cwd: Some("/home/user/project".to_string()),
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 10);
        if let Message::CompletionRequest(req) = decoded.payload {
            assert_eq!(req.input, "git sta");
            assert_eq!(req.sequence_id, 42);
            assert_eq!(req.cwd, Some("/home/user/project".to_string()));
        } else {
            panic!("expected CompletionRequest");
        }
    }

    #[test]
    fn test_frame_with_completion_response() {
        let frame = Frame {
            request_id: 11,
            payload: Message::CompletionResponse(CompletionResponse {
                sequence_id: 42,
                suggestions: vec![
                    CompletionSuggestion {
                        text: "tus".to_string(),
                        confidence: 0.95,
                    },
                ],
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 11);
        if let Message::CompletionResponse(resp) = decoded.payload {
            assert_eq!(resp.sequence_id, 42);
            assert_eq!(resp.suggestions.len(), 1);
            assert_eq!(resp.suggestions[0].text, "tus");
        } else {
            panic!("expected CompletionResponse");
        }
    }

    #[test]
    fn test_frame_with_session_update() {
        let mut attrs = HashMap::new();
        attrs.insert("host".to_string(), "workstation".to_string());
        attrs.insert("shell_cwd".to_string(), "/home/user/project".to_string());
        attrs.insert("child_process".to_string(), "vim:12345".to_string());
        let frame = Frame {
            request_id: 20,
            payload: Message::SessionUpdate(SessionUpdate {
                session_id: "abc".to_string(),
                timestamp_ms: 2000,
                attrs,
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 20);
        if let Message::SessionUpdate(su) = decoded.payload {
            assert_eq!(su.session_id, "abc");
            assert_eq!(su.attrs.get("shell_cwd").unwrap(), "/home/user/project");
        } else {
            panic!("expected SessionUpdate");
        }
    }

    #[test]
    fn test_frame_with_chat_start() {
        let frame = Frame {
            request_id: 30,
            payload: Message::ChatStart(ChatStart {
                request_id: "abc".to_string(),
                session_id: "sess1".to_string(),
                new_thread: false,
                thread_id: None,
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 30);
        assert!(matches!(decoded.payload, Message::ChatStart(_)));
    }

    #[test]
    fn test_frame_with_chat_tool_status() {
        let frame = Frame {
            request_id: 40,
            payload: Message::ChatToolStatus(ChatToolStatus {
                request_id: "req1".to_string(),
                thread_id: "thread1".to_string(),
                tool_name: "command_query".to_string(),
                status: "查询命令历史...".to_string(),
                tool_call_id: Some("tc_001".to_string()),
                status_icon: Some(StatusIcon::Running),
                display_name: Some("Command Query".to_string()),
                param_desc: Some("pattern=git".to_string()),
                result_compact: None,
                result_full: None,
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 40);
        if let Message::ChatToolStatus(cts) = decoded.payload {
            assert_eq!(cts.tool_name, "command_query");
            assert_eq!(cts.status, "查询命令历史...");
            assert_eq!(cts.tool_call_id, Some("tc_001".to_string()));
            assert!(matches!(cts.status_icon, Some(StatusIcon::Running)));
            assert_eq!(cts.display_name, Some("Command Query".to_string()));
            assert_eq!(cts.param_desc, Some("pattern=git".to_string()));
            assert!(cts.result_compact.is_none());
            assert!(cts.result_full.is_none());
        } else {
            panic!("expected ChatToolStatus");
        }
    }

    #[test]
    fn test_frame_with_chat_message() {
        let frame = Frame {
            request_id: 31,
            payload: Message::ChatMessage(ChatMessage {
                request_id: "def".to_string(),
                session_id: "sess1".to_string(),
                thread_id: "thread-uuid".to_string(),
                query: "hello".to_string(),
                model: None,
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, 31);
        if let Message::ChatMessage(cm) = decoded.payload {
            assert_eq!(cm.query, "hello");
            assert_eq!(cm.thread_id, "thread-uuid");
        } else {
            panic!("expected ChatMessage");
        }
    }

    /// Guard test: exhaustive match with no wildcard.
    /// Adding a new Message variant will cause a compile error here,
    /// reminding you to bump PROTOCOL_VERSION if the wire format changed.
    #[test]
    fn message_variant_guard() {
        const EXPECTED_VARIANT_COUNT: usize = 33;

        let variants: Vec<Message> = vec![
            Message::SessionStart(SessionStart {
                session_id: String::new(),
                parent_session_id: None,
                timestamp_ms: 0,
                attrs: HashMap::new(),
            }),
            Message::SessionEnd(SessionEnd {
                session_id: String::new(),
                timestamp_ms: 0,
                exit_code: None,
            }),
            Message::SessionUpdate(SessionUpdate {
                session_id: String::new(),
                timestamp_ms: 0,
                attrs: HashMap::new(),
            }),
            Message::IoData(IoData {
                session_id: String::new(),
                direction: IoDirection::Input,
                timestamp_ms: 0,
                data: vec![],
            }),
            Message::Event(Event {
                session_id: String::new(),
                timestamp_ms: 0,
                event_type: EventType::NonZeroExit(1),
            }),
            Message::Request(Request {
                request_id: String::new(),
                session_id: String::new(),
                query: String::new(),
                scope: RequestScope::CurrentSession,
            }),
            Message::Response(Response {
                request_id: String::new(),
                content: String::new(),
                is_streaming: false,
                is_final: true,
            }),
            Message::CommandComplete(CommandComplete {
                session_id: String::new(),
                record: omnish_store::command::CommandRecord {
                    command_id: String::new(),
                    session_id: String::new(),
                    command_line: None,
                    cwd: None,
                    started_at: 0,
                    ended_at: None,
                    output_summary: String::new(),
                    stream_offset: 0,
                    stream_length: 0,
                    exit_code: None,
                },
            }),
            Message::CompletionRequest(CompletionRequest {
                session_id: String::new(),
                input: String::new(),
                cursor_pos: 0,
                sequence_id: 0,
                cwd: None,
            }),
            Message::CompletionResponse(CompletionResponse {
                sequence_id: 0,
                suggestions: vec![],
            }),
            Message::CompletionSummary(CompletionSummary {
                session_id: String::new(),
                sequence_id: 0,
                prompt: String::new(),
                completion: String::new(),
                accepted: false,
                latency_ms: 0,
                dwell_time_ms: None,
                cwd: None,
                extra: HashMap::new(),
            }),
            Message::ChatStart(ChatStart {
                request_id: String::new(),
                session_id: String::new(),
                new_thread: false,
                thread_id: None,
            }),
            Message::ChatReady(ChatReady {
                request_id: String::new(),
                thread_id: String::new(),
                last_exchange: None,
                earlier_count: 0,
                model_name: None,
                history: None,
                thread_host: None,
                thread_cwd: None,
                thread_summary: None,
                error: None,
                error_display: None,
            }),
            Message::ChatEnd(ChatEnd {
                session_id: String::new(),
                thread_id: String::new(),
            }),
            Message::ChatMessage(ChatMessage {
                request_id: String::new(),
                session_id: String::new(),
                thread_id: String::new(),
                query: String::new(),
                model: None,
            }),
            Message::ChatResponse(ChatResponse {
                request_id: String::new(),
                thread_id: String::new(),
                content: String::new(),
            }),
            Message::ChatInterrupt(ChatInterrupt {
                request_id: String::new(),
                session_id: String::new(),
                thread_id: String::new(),
                query: String::new(),
            }),
            Message::ChatToolStatus(ChatToolStatus {
                request_id: String::new(),
                thread_id: String::new(),
                tool_name: String::new(),
                status: String::new(),
                tool_call_id: None,
                status_icon: None,
                display_name: None,
                param_desc: None,
                result_compact: None,
                result_full: None,
            }),
            Message::ChatToolCall(ChatToolCall {
                request_id: String::new(),
                thread_id: String::new(),
                tool_name: String::new(),
                tool_call_id: String::new(),
                input: String::new(),
                plugin_name: String::new(),
                sandboxed: true,
            }),
            Message::ChatToolResult(ChatToolResult {
                request_id: String::new(),
                thread_id: String::new(),
                tool_call_id: String::new(),
                content: String::new(),
                is_error: false,
                needs_summarization: false,
            }),
            Message::Ack,
            Message::Auth(Auth {
                token: String::new(),
                protocol_version: 0,
            }),
            Message::AuthResult(AuthResult {
                ok: true,
                protocol_version: 0,
                daemon_version: String::new(),
            }),
            Message::ConfigQuery,
            Message::ConfigResponse { items: vec![], handlers: vec![] },
            Message::ConfigUpdate { changes: vec![] },
            Message::ConfigUpdateResult { ok: true, error: None },
            Message::UpdateCheck { os: "linux".into(), arch: "x86_64".into(), current_version: "0.1.0".into(), hostname: "host1".into() },
            Message::UpdateInfo { latest_version: "0.2.0".into(), checksum: "abc123".into(), available: true },
            Message::UpdateRequest { os: "linux".into(), arch: "x86_64".into(), version: "0.2.0".into(), hostname: "host1".into() },
            Message::UpdateChunk { seq: 0, total_size: 1024, checksum: "abc".into(), data: vec![1,2,3], done: false, error: None },
            Message::ConfigClient { changes: vec![] },
            Message::TestDisconnect { delay_secs: 5 },
        ];

        // Exhaustive match — no wildcard. Compiler will error if a variant is missing.
        for v in &variants {
            match v {
                Message::SessionStart(_)
                | Message::SessionEnd(_)
                | Message::SessionUpdate(_)
                | Message::IoData(_)
                | Message::Event(_)
                | Message::Request(_)
                | Message::Response(_)
                | Message::CommandComplete(_)
                | Message::CompletionRequest(_)
                | Message::CompletionResponse(_)
                | Message::CompletionSummary(_)
                | Message::ChatStart(_)
                | Message::ChatReady(_)
                | Message::ChatEnd(_)
                | Message::ChatMessage(_)
                | Message::ChatResponse(_)
                | Message::ChatInterrupt(_)
                | Message::ChatToolStatus(_)
                | Message::ChatToolCall(_)
                | Message::ChatToolResult(_)
                | Message::Ack
                | Message::Auth(_)
                | Message::AuthResult(_)
                | Message::ConfigQuery
                | Message::ConfigResponse { .. }
                | Message::ConfigUpdate { .. }
                | Message::ConfigUpdateResult { .. }
                | Message::ConfigClient { .. }
                | Message::UpdateCheck { .. }
                | Message::UpdateInfo { .. }
                | Message::UpdateRequest { .. }
                | Message::UpdateChunk { .. }
                | Message::TestDisconnect { .. } => {}
            }
        }

        assert_eq!(
            variants.len(),
            EXPECTED_VARIANT_COUNT,
            "Message variant count changed! If you added/removed a variant, \
             update EXPECTED_VARIANT_COUNT and consider bumping PROTOCOL_VERSION."
        );
    }

    /// Bincode serializes enums as a u32 variant index. New variants MUST be
    /// appended at the end of the enum — inserting in the middle shifts indices
    /// and breaks old clients that haven't upgraded yet.
    ///
    /// This test pins the variant indices of critical message types so that
    /// inserting a variant in the middle causes a compile-time-like failure.
    #[test]
    fn variant_indices_are_stable() {
        fn variant_index(msg: &Message) -> u32 {
            let bytes = bincode::serialize(msg).expect("serialize");
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }

        // Auth / AuthResult — must be stable for handshake to work across versions
        assert_eq!(variant_index(&Message::Auth(Auth { token: String::new(), protocol_version: 0 })), 21, "Auth index shifted");
        assert_eq!(variant_index(&Message::AuthResult(AuthResult { ok: true, protocol_version: 0, daemon_version: String::new() })), 22, "AuthResult index shifted");

        // Update messages — old clients rely on these indices for self-update
        assert_eq!(variant_index(&Message::UpdateCheck { os: String::new(), arch: String::new(), current_version: String::new(), hostname: String::new() }), 27, "UpdateCheck index shifted");
        assert_eq!(variant_index(&Message::UpdateInfo { latest_version: String::new(), checksum: String::new(), available: false }), 28, "UpdateInfo index shifted");
        assert_eq!(variant_index(&Message::UpdateRequest { os: String::new(), arch: String::new(), version: String::new(), hostname: String::new() }), 29, "UpdateRequest index shifted");
        assert_eq!(variant_index(&Message::UpdateChunk { seq: 0, total_size: 0, checksum: String::new(), data: vec![], done: false, error: None }), 30, "UpdateChunk index shifted");

        // New variants must go at the end — ConfigClient was the first such addition
        assert_eq!(variant_index(&Message::ConfigClient { changes: vec![] }), 31, "ConfigClient index shifted");
    }

    /// Regression test: ChatReady with populated history must survive a bincode round-trip.
    ///
    /// Previously, `history` was `Option<Vec<serde_json::Value>>`, which caused
    /// `bincode` to fail with "does not support deserialize_any", silently dropping
    /// the ChatReady frame on the client and causing a 15-second timeout on resume.
    /// `history` is now `Option<Vec<String>>` (JSON-encoded entries). This test
    /// will fail immediately if someone reverts that change.
    #[test]
    fn chat_ready_with_history_round_trips() {
        let frame = Frame {
            request_id: 99,
            payload: Message::ChatReady(ChatReady {
                request_id: "abc".to_string(),
                thread_id: "tid-1".to_string(),
                last_exchange: None,
                earlier_count: 2,
                model_name: Some("claude-sonnet".to_string()),
                history: Some(vec![
                    r#"{"type":"user_input","text":"hello"}"#.to_string(),
                    r#"{"type":"response","text":"hi there"}"#.to_string(),
                ]),
                thread_host: Some("fortress".to_string()),
                thread_cwd: Some("/home/user/project".to_string()),
                thread_summary: Some("Discussed project setup".to_string()),
                error: None,
                error_display: None,
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        if let Message::ChatReady(ready) = decoded.payload {
            let h = ready.history.expect("history should be Some");
            assert_eq!(h.len(), 2);
            assert!(h[0].contains("user_input"));
            assert!(h[1].contains("hi there"));
        } else {
            panic!("expected ChatReady");
        }
    }

    /// Regression guard: CompletionSummary.extra must survive a bincode round-trip
    /// even when non-empty. The field type is `HashMap<String, String>` (not Value)
    /// for the same reason as ChatReady.history.
    #[test]
    fn completion_summary_extra_round_trips() {
        let mut extra = HashMap::new();
        extra.insert("model".to_string(), "claude-sonnet".to_string());
        extra.insert("cache_hit".to_string(), "true".to_string());
        let frame = Frame {
            request_id: 1,
            payload: Message::CompletionSummary(CompletionSummary {
                session_id: "s1".to_string(),
                sequence_id: 7,
                prompt: "cd w".to_string(),
                completion: "cd workspace".to_string(),
                accepted: true,
                latency_ms: 120,
                dwell_time_ms: Some(500),
                cwd: Some("/home/user".to_string()),
                extra,
            }),
        };
        let bytes = frame.to_bytes().unwrap();
        let decoded = Frame::from_bytes(&bytes).unwrap();
        if let Message::CompletionSummary(cs) = decoded.payload {
            assert_eq!(cs.extra.get("model").map(|s| s.as_str()), Some("claude-sonnet"));
            assert_eq!(cs.extra.len(), 2);
        } else {
            panic!("expected CompletionSummary");
        }
    }
}
