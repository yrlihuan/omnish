use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

const MAGIC: [u8; 2] = [0x4F, 0x53]; // "OS" for OmniSh

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
    Ack,
    Auth(Auth),
    AuthFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    pub token: String,
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
    /// Extra metadata as key-value pairs (stored as JSON in CSV)
    #[serde(default)]
    pub extra: HashMap<String, Value>,
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
}
