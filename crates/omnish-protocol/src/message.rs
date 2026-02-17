use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAGIC: [u8; 2] = [0x4F, 0x53]; // "OS" for OmniSh

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    SessionStart(SessionStart),
    SessionEnd(SessionEnd),
    IoData(IoData),
    Event(Event),
    Request(Request),
    Response(Response),
    CommandComplete(CommandComplete),
    Ack,
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
}
